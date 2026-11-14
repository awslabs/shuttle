//! The simulated kernel: one ring's queues, the ops it holds in flight, and
//! the Shuttle task that completes them.
//!
//! Each ring has a kernel task. While ops are in flight it is runnable, and
//! each time Shuttle schedules it, it completes one of them, chosen with
//! `shuttle::rand`, by running the op's system call against the real file
//! descriptor. An op therefore stays in flight for as long as Shuttle keeps
//! the task off the CPU, ops complete in any order, and a schedule replays
//! both. Buffers behave as they do with the kernel: a read writes the
//! caller's buffer, and a write reads it, when the op completes. A vectored
//! op's `iovec` array is copied when the op is submitted, as the kernel does
//! with `IORING_FEAT_SUBMIT_STABLE`; the buffers it lists, at completion.
//!
//! Posting a completion follows the kernel's order (`io_uring/eventfd.c`):
//! store the CQ tail, then load the CQ flags and signal the registered
//! eventfd unless `IORING_CQ_EVENTFD_DISABLED` is set. Both words are Shuttle
//! atomics, so a consumer that clears the flag and then loads the tail races
//! the kernel at every interleaving Shuttle can choose.

use std::collections::VecDeque;
use std::future::poll_fn;
use std::io;
use std::mem;
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::thread;

use shuttle::current::{self, TaskName};
use shuttle::future;
use shuttle::rand::{Rng, thread_rng};
use shuttle::sync::atomic::{AtomicU32, Ordering};

use crate::async_fd::{self, Readiness};
use crate::buffers;
use crate::cqueue;
use crate::faults;
use crate::opcode;
use crate::squeue::{self, Flags, SqOp};
use crate::types::Timespec;
use crate::types::sealed::Target;
use crate::version::{Kernel, since};

/// `IORING_CQ_EVENTFD_DISABLED`.
const CQ_EVENTFD_DISABLED: u32 = 1;

/// How often an op completes inline, during the enter that submits it,
/// instead of going in flight. The kernel does this whenever an op can finish
/// without waiting; here it lets a consumer find a completion its own submit
/// just posted.
const INLINE_COMPLETION: f64 = 0.25;

/// The block a refused read or write moves part of its range in: the
/// largest logical block size, so the part stays aligned for `O_DIRECT`.
const PART_BLOCK: u32 = 4096;

/// One simulated ring.
pub(crate) struct Ring {
    /// The ring's (simulated) file descriptor.
    pub(crate) fd: RawFd,
    pub(crate) sq_entries: u32,
    pub(crate) cq_entries: u32,
    /// Bookkeeping no consumer can observe directly, behind a plain lock so
    /// it adds no scheduling points of its own.
    state: Mutex<State>,
    /// Completions posted: the kernel stores it after writing a completion,
    /// and a consumer's `sync` loads it.
    cq_tail: AtomicU32,
    /// The CQ ring's flag word.
    cq_flags: AtomicU32,
    /// Readiness of the ring fd itself, for an `AsyncFd` on it. Every post
    /// sets it, whatever the eventfd flag says, as polling the ring fd does.
    readiness: Arc<Readiness>,
    /// The kernel the ring behaves as, fixed when it is created.
    pub(crate) kernel: Kernel,
}

struct State {
    /// Published SQEs the kernel has not consumed.
    sq: VecDeque<squeue::Entry>,
    /// Ops the kernel holds: submitted and not yet completed.
    inflight: Vec<InFlight>,
    /// Completions posted and not yet taken.
    cq: VecDeque<cqueue::Entry>,
    /// Completions that found the CQ full. With `IORING_FEAT_NODROP` the
    /// kernel holds them back and flushes them on the next enter.
    overflow: VecDeque<cqueue::Entry>,
    /// The kernel has `IORING_FEAT_NODROP`; without it, a completion that
    /// finds the CQ full is dropped and counted in `dropped`.
    nodrop: bool,
    dropped: u32,
    /// Completions consumers have taken.
    cq_head: u32,
    /// The CQ tail as of the kernel's last store: what an enter waiting for
    /// completions counts, so it never wakes for one a consumer cannot see.
    published: u32,
    /// Completions ever posted, which a timeout with a count counts.
    posted: u64,
    /// The registered-file table, once registered.
    files: Option<Vec<RawFd>>,
    /// The registered eventfd.
    eventfd: Option<Eventfd>,
    /// The kernel task, parked while nothing is in flight.
    kernel: Option<Waker>,
    /// Tasks blocked in an enter until completions post.
    waiters: Vec<Waiter>,
    next_waiter_id: u64,
    /// A timer has fired with I/O outstanding: the device was seen to hang,
    /// so the owner may drop the ring with ops in flight, leaking their
    /// buffers (see [`Ring::close`]).
    hung: bool,
    closed: bool,
}

/// An eventfd registered with the ring.
struct Eventfd {
    fd: RawFd,
    /// The readiness an `AsyncFd` on the fd sees.
    readiness: Arc<Readiness>,
    /// Registered with `IORING_REGISTER_EVENTFD_ASYNC`: signalled only for
    /// completions that did not post inline, during the enter that submitted
    /// their op.
    async_only: bool,
}

/// A task blocked in an enter, waiting for completions.
struct Waiter {
    id: u64,
    waker: Waker,
    /// The enter has a timeout, so the kernel may expire it rather than
    /// complete an op: it is a timer among the ops in flight.
    timed: bool,
    /// The kernel fired this wait's timeout.
    expired: bool,
}

/// What the kernel task does next.
enum Next {
    /// Complete this op.
    Op(InFlight),
    /// A timed enter's timeout fired; the waiter is already woken.
    WaitExpired,
}

/// The in-flight thing the kernel picked to deal with.
enum Pick {
    /// The op at this index of `inflight`.
    Op(usize),
    /// The timeout of the timed enter waiting as this id.
    WaitTimer(u64),
}

struct InFlight {
    user_data: u64,
    op: Op,
    /// A read or write submitted without `IOSQE_ASYNC`, which the kernel may
    /// refuse (see [`faults::Faults::eagain`]).
    refusable: bool,
    /// The op's buffers, watched while the kernel owns them (see
    /// [`buffers::Checks`]).
    watch: Option<buffers::Watch>,
}

impl InFlight {
    /// Complete the op: run its system call, or refuse it with `-EAGAIN` at
    /// the rate this thread's faults set, after moving part of its range; or
    /// post one of the errors this thread's errors set (see
    /// [`faults::Errors`]).
    ///
    /// Checks the op's buffers first, so a buffer the submitter touched while
    /// the kernel owned it panics here rather than surfacing as the wrong
    /// bytes later.
    fn complete(self) -> i32 {
        let InFlight {
            user_data,
            mut op,
            refusable,
            watch,
        } = self;
        op.read_iovecs_again(user_data);
        if let Some(watch) = &watch {
            watch.before_completion();
        }
        if refusable && faults::refuse() {
            op.move_part();
            return -libc::EAGAIN;
        }
        match op {
            Op::Read { .. } | Op::Readv { .. } if faults::fail_read() => {
                op.move_part();
                return -libc::EIO;
            }
            Op::Read { .. } | Op::Readv { .. } if faults::short_read() => {
                let moved = op.move_part();
                if moved > 0 {
                    faults::note_short_read();
                    return moved as i32;
                }
            }
            Op::Fallocate { .. } if faults::no_space() => return -libc::ENOSPC,
            _ => {}
        }
        op.execute()
    }
}

/// An op the kernel holds, its file resolved at submission as the kernel
/// takes its file reference then.
enum Op {
    Nop,
    Read {
        fd: RawFd,
        buf: *mut u8,
        len: u32,
        offset: u64,
        rw_flags: i32,
    },
    Write {
        fd: RawFd,
        buf: *const u8,
        len: u32,
        offset: u64,
        rw_flags: i32,
    },
    /// The `iovec` array is the kernel's copy, taken at submission.
    Readv {
        fd: RawFd,
        iovecs: Vec<libc::iovec>,
        offset: u64,
        rw_flags: i32,
        /// The submitter's array, for a kernel without
        /// `IORING_FEAT_SUBMIT_STABLE` to read again when it runs the op.
        source: Option<(*const libc::iovec, u32)>,
    },
    Writev {
        fd: RawFd,
        iovecs: Vec<libc::iovec>,
        offset: u64,
        rw_flags: i32,
        /// The submitter's array, for a kernel without
        /// `IORING_FEAT_SUBMIT_STABLE` to read again when it runs the op.
        source: Option<(*const libc::iovec, u32)>,
    },
    Fsync {
        fd: RawFd,
        datasync: bool,
    },
    Fallocate {
        fd: RawFd,
        offset: u64,
        len: u64,
        mode: i32,
    },
    /// A timer; with a non-zero `count`, also met once `count` completions
    /// have posted after `since`.
    Timeout {
        count: u32,
        since: u64,
    },
}

// SAFETY: as for `SqOp`: the buffers are the submitter's, valid and
// unaliased until the completion is reaped, and only the task completing
// the op touches them.
unsafe impl Send for Op {}

impl Op {
    /// Move a part of a read's or write's range, whole blocks short of all
    /// of it, as one the kernel refuses may have before it gave up, and
    /// return the bytes moved; nothing for any other op.
    fn move_part(&self) -> usize {
        let Some(len) = self.transfer_len() else {
            return 0;
        };
        // Drawn as a `u32`, as before vectored ops, so recorded schedules of
        // single-buffer ops still replay.
        let blocks = u32::try_from(len / PART_BLOCK as usize).unwrap_or(u32::MAX);
        if blocks == 0 {
            return 0;
        }
        let part = thread_rng().gen_range(0..blocks) as usize * PART_BLOCK as usize;
        if part == 0 {
            return 0;
        }
        // SAFETY: as for `execute`, and `part` is shorter than the buffers.
        // A part that fails moved nothing, which a refusal also allows.
        let moved = unsafe {
            match self {
                Op::Read {
                    fd,
                    buf,
                    offset,
                    rw_flags,
                    ..
                } => read_buf(*fd, *buf, part, *offset, *rw_flags),
                Op::Write {
                    fd,
                    buf,
                    offset,
                    rw_flags,
                    ..
                } => write_buf(*fd, *buf, part, *offset, *rw_flags),
                Op::Readv {
                    fd,
                    iovecs,
                    offset,
                    rw_flags,
                    ..
                } => read_iov(*fd, &prefix(iovecs, part), *offset, *rw_flags),
                Op::Writev {
                    fd,
                    iovecs,
                    offset,
                    rw_flags,
                    ..
                } => write_iov(*fd, &prefix(iovecs, part), *offset, *rw_flags),
                _ => unreachable!("only reads and writes move a part"),
            }
        };
        moved.max(0) as usize
    }

    /// For a kernel without `IORING_FEAT_SUBMIT_STABLE` (before 5.5), read a
    /// vectored op's `iovec` array again, as that kernel does when it runs the
    /// op rather than when it is submitted.
    ///
    /// Panics if the array changed while the op was in flight, when the
    /// buffer checks are on (see [`buffers::Checks`]): on such a kernel the
    /// array must outlive the op, not just the submit.
    fn read_iovecs_again(&mut self, user_data: u64) {
        let (Op::Readv {
            iovecs,
            source: Some((array, count)),
            ..
        }
        | Op::Writev {
            iovecs,
            source: Some((array, count)),
            ..
        }) = self
        else {
            return;
        };
        let now = import_iovecs(*array, *count).expect("checked when the op was submitted");
        if iovec_bufs(&now) == iovec_bufs(iovecs) {
            return;
        }
        let checks = buffers::checks();
        assert!(
            !(checks.reads || checks.writes),
            "shuttle-io-uring: op {user_data}'s iovec array changed while it was in flight: \
             {:?} at submit, {:?} now; a kernel without IORING_FEAT_SUBMIT_STABLE (before 5.5) \
             reads it when it runs the op",
            iovec_bufs(iovecs),
            iovec_bufs(&now),
        );
        *iovecs = now;
    }

    /// The bytes a read or write moves when it completes in full; `None` for
    /// any other op.
    fn transfer_len(&self) -> Option<usize> {
        match self {
            Op::Read { len, .. } | Op::Write { len, .. } => Some(*len as usize),
            Op::Readv { iovecs, .. } | Op::Writev { iovecs, .. } => Some(iovecs.iter().map(|iov| iov.iov_len).sum()),
            _ => None,
        }
    }

    /// Watch the op's buffers for as long as the kernel owns them. `None` for
    /// an op with no buffers and no file range, and when every check is off.
    fn watch(&self, user_data: u64) -> Option<buffers::Watch> {
        let (access, fd, bufs, offset) = match self {
            Op::Read {
                fd, buf, len, offset, ..
            } => (
                buffers::Access::Read,
                *fd,
                vec![(*buf as usize, *len as usize)],
                *offset,
            ),
            Op::Write {
                fd, buf, len, offset, ..
            } => (
                buffers::Access::Write,
                *fd,
                vec![(*buf as usize, *len as usize)],
                *offset,
            ),
            Op::Readv { fd, iovecs, offset, .. } => (buffers::Access::Read, *fd, iovec_bufs(iovecs), *offset),
            Op::Writev { fd, iovecs, offset, .. } => (buffers::Access::Write, *fd, iovec_bufs(iovecs), *offset),
            // Changes a file's bytes with no buffer of its own, so only its
            // file range is watched.
            Op::Fallocate { fd, offset, .. } => (buffers::Access::Write, *fd, Vec::new(), *offset),
            Op::Nop | Op::Fsync { .. } | Op::Timeout { .. } => return None,
        };
        let file = match self {
            Op::Fallocate { offset, len, .. } => Some(*offset..offset.saturating_add(*len)),
            // At the file position: which bytes is known only when it runs.
            _ if offset == CUR_POS => None,
            _ => self.transfer_len().map(|len| offset..offset.saturating_add(len as u64)),
        };
        buffers::watch(user_data, fd, access, offset, &bufs, file)
    }

    /// Run the op's system call and return its CQE result.
    fn execute(self) -> i32 {
        // SAFETY: the io_uring contract: the submitter keeps each buffer
        // valid, and does not touch it, until the op's completion is reaped.
        let ret: isize = unsafe {
            match self {
                Op::Nop => 0,
                Op::Read {
                    fd,
                    buf,
                    len,
                    offset,
                    rw_flags,
                } => read_buf(fd, buf, len as usize, offset, rw_flags),
                Op::Write {
                    fd,
                    buf,
                    len,
                    offset,
                    rw_flags,
                } => write_buf(fd, buf, len as usize, offset, rw_flags),
                Op::Readv {
                    fd,
                    iovecs,
                    offset,
                    rw_flags,
                    ..
                } => read_iov(fd, &iovecs, offset, rw_flags),
                Op::Writev {
                    fd,
                    iovecs,
                    offset,
                    rw_flags,
                    ..
                } => write_iov(fd, &iovecs, offset, rw_flags),
                Op::Fsync { fd, datasync } => (if datasync { libc::fdatasync(fd) } else { libc::fsync(fd) }) as isize,
                Op::Fallocate { fd, offset, len, mode } => {
                    libc::fallocate(fd, mode, offset as libc::off_t, len as libc::off_t) as isize
                }
                // The kernel task completes a timer when its timeout fires.
                Op::Timeout { .. } => return -libc::ETIME,
            }
        };
        if ret < 0 {
            -io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
        } else {
            ret as i32
        }
    }
}

/// An op's offset of −1: the file's position, which the op uses and advances
/// (`IORING_FEAT_RW_CUR_POS`).
const CUR_POS: u64 = u64::MAX;

/// Read into `iov` at `offset` as the kernel would: `preadv`, or `preadv2`
/// for per-op flags or the file position, which only it takes.
///
/// # Safety
///
/// The io_uring contract: every buffer `iov` lists is valid for writes of its
/// length, and untouched by the submitter until the completion is reaped.
unsafe fn read_iov(fd: RawFd, iov: &[libc::iovec], offset: u64, rw_flags: i32) -> isize {
    let count = iov.len() as libc::c_int;
    // SAFETY: as the caller promises.
    unsafe {
        if rw_flags == 0 && offset != CUR_POS {
            libc::preadv(fd, iov.as_ptr(), count, offset as libc::off_t)
        } else {
            libc::preadv2(fd, iov.as_ptr(), count, offset as libc::off_t, rw_flags)
        }
    }
}

/// Write `iov` at `offset`, as [`read_iov`] reads.
///
/// # Safety
///
/// As for [`read_iov`], with the buffers valid for reads.
unsafe fn write_iov(fd: RawFd, iov: &[libc::iovec], offset: u64, rw_flags: i32) -> isize {
    let count = iov.len() as libc::c_int;
    // SAFETY: as the caller promises.
    unsafe {
        if rw_flags == 0 && offset != CUR_POS {
            libc::pwritev(fd, iov.as_ptr(), count, offset as libc::off_t)
        } else {
            libc::pwritev2(fd, iov.as_ptr(), count, offset as libc::off_t, rw_flags)
        }
    }
}

/// Read `len` bytes into `buf`: `pread`, or as [`read_iov`] for flags or the
/// file position.
///
/// # Safety
///
/// As for [`read_iov`].
unsafe fn read_buf(fd: RawFd, buf: *mut u8, len: usize, offset: u64, rw_flags: i32) -> isize {
    // SAFETY: as the caller promises.
    unsafe {
        if rw_flags == 0 && offset != CUR_POS {
            libc::pread(fd, buf.cast(), len, offset as libc::off_t)
        } else {
            let iov = libc::iovec {
                iov_base: buf.cast(),
                iov_len: len,
            };
            read_iov(fd, &[iov], offset, rw_flags)
        }
    }
}

/// Write `len` bytes from `buf`, as [`read_buf`] reads.
///
/// # Safety
///
/// As for [`write_iov`].
unsafe fn write_buf(fd: RawFd, buf: *const u8, len: usize, offset: u64, rw_flags: i32) -> isize {
    // SAFETY: as the caller promises.
    unsafe {
        if rw_flags == 0 && offset != CUR_POS {
            libc::pwrite(fd, buf.cast(), len, offset as libc::off_t)
        } else {
            let iov = libc::iovec {
                iov_base: buf as *mut libc::c_void,
                iov_len: len,
            };
            write_iov(fd, &[iov], offset, rw_flags)
        }
    }
}

/// Copy a vectored op's `iovec` array, as the kernel's import of it does when
/// the op is submitted: more than `UIO_MAXIOV` entries is `EINVAL`, and a null
/// array of any is `EFAULT`.
fn import_iovecs(iovec: *const libc::iovec, count: u32) -> Result<Vec<libc::iovec>, i32> {
    if count > libc::UIO_MAXIOV as u32 {
        return Err(libc::EINVAL);
    }
    if count == 0 {
        return Ok(Vec::new());
    }
    if iovec.is_null() {
        return Err(libc::EFAULT);
    }
    // SAFETY: the io_uring contract: the submitter keeps the array valid
    // until the entry is submitted, which is now.
    Ok(unsafe { std::slice::from_raw_parts(iovec, count as usize) }.to_vec())
}

/// Add one to a registered eventfd's counter, as the kernel's
/// `eventfd_signal` does, so a consumer that reads the fd to reset it reads
/// the completions out rather than blocking on an empty counter.
///
/// Skipped when the fd cannot take the write, which a counter at its maximum
/// cannot: the kernel's own `eventfd_signal` adds nothing then either.
fn add_to_eventfd(fd: RawFd) {
    let one: u64 = 1;
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: plain syscalls on the fd the consumer registered, which it keeps
    // open until it unregisters it. An fd that is not an eventfd takes the
    // eight bytes as an ordinary write, as it would from the kernel.
    unsafe {
        if libc::poll(&mut pollfd, 1, 0) > 0 && pollfd.revents & libc::POLLOUT != 0 {
            libc::write(fd, (&raw const one).cast(), 8);
        }
    }
}

/// Each `iovec`'s address and length.
fn iovec_bufs(iovecs: &[libc::iovec]) -> Vec<(usize, usize)> {
    iovecs.iter().map(|iov| (iov.iov_base as usize, iov.iov_len)).collect()
}

/// The first `bytes` of `iovecs`: whole entries, then the one `bytes` ends
/// in, trimmed.
fn prefix(iovecs: &[libc::iovec], mut bytes: usize) -> Vec<libc::iovec> {
    let mut part = Vec::new();
    for iov in iovecs {
        if bytes == 0 {
            break;
        }
        let len = iov.iov_len.min(bytes);
        part.push(libc::iovec {
            iov_base: iov.iov_base,
            iov_len: len,
        });
        bytes -= len;
    }
    part
}

impl Ring {
    /// A new ring and its kernel task.
    pub(crate) fn start(sq_entries: u32, cq_entries: u32, kernel: Kernel) -> Arc<Ring> {
        let fd = async_fd::alloc_ring_fd();
        let ring = Arc::new(Ring {
            fd,
            sq_entries,
            cq_entries,
            state: Mutex::new(State {
                sq: VecDeque::new(),
                inflight: Vec::new(),
                cq: VecDeque::new(),
                overflow: VecDeque::new(),
                nodrop: kernel.has(since::NODROP),
                dropped: 0,
                cq_head: 0,
                published: 0,
                posted: 0,
                files: None,
                eventfd: None,
                kernel: None,
                waiters: Vec::new(),
                next_waiter_id: 0,
                hung: false,
                closed: false,
            }),
            cq_tail: AtomicU32::new(0),
            cq_flags: AtomicU32::new(0),
            readiness: Arc::new(Readiness::new()),
            kernel,
        });
        async_fd::register(fd, Arc::clone(&ring.readiness));
        let kernel = Arc::clone(&ring);
        // Detached: the task ends once the ring is closed.
        drop(future::spawn(async move {
            current::set_label_for_task(
                current::me(),
                TaskName::from(format!("io_uring kernel (ring {})", kernel.fd)),
            );
            while let Some(next) = poll_fn(|cx| kernel.poll_next(cx)).await {
                if let Next::Op(op) = next {
                    let user_data = op.user_data;
                    let result = op.complete();
                    kernel.post(user_data, result, false);
                }
                future::yield_now().await;
            }
        }));
        ring
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Take the next op to complete or timed enter to expire, chosen by
    /// Shuttle, or park until one arrives. Ends the kernel task once the ring
    /// is closed.
    fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Option<Next>> {
        let mut state = self.lock();
        if state.closed {
            return Poll::Ready(None);
        }
        if state.inflight.is_empty() && state.wait_timers().is_empty() {
            state.kernel = Some(cx.waker().clone());
            return Poll::Pending;
        }
        match state.pick_next() {
            Pick::Op(i) => {
                let op = state.inflight.swap_remove(i);
                if matches!(op.op, Op::Timeout { .. }) && state.io_in_flight() {
                    state.hung = true;
                }
                Poll::Ready(Some(Next::Op(op)))
            }
            Pick::WaitTimer(id) => {
                if state.io_in_flight() {
                    state.hung = true;
                }
                let waker = state.waiters.iter_mut().find(|waiter| waiter.id == id).map(|waiter| {
                    waiter.expired = true;
                    waiter.waker.clone()
                });
                drop(state);
                if let Some(waker) = waker {
                    waker.wake();
                }
                Poll::Ready(Some(Next::WaitExpired))
            }
        }
    }

    // ---- submission queue ----

    /// Make entries a queue handle pushed visible to the kernel.
    pub(crate) fn publish_sqes(&self, entries: Vec<squeue::Entry>) {
        self.lock().sq.extend(entries);
    }

    /// Published entries the kernel has not consumed.
    pub(crate) fn sq_published(&self) -> usize {
        self.lock().sq.len()
    }

    // ---- completion queue ----

    /// Completions visible to a consumer that loads the tail now. Loading the
    /// tail is a scheduling point.
    pub(crate) fn cq_visible(&self) -> u32 {
        let tail = self.cq_tail.load(Ordering::SeqCst);
        tail.wrapping_sub(self.lock().cq_head)
    }

    /// Take the oldest posted completion. Only called for a completion a tail
    /// load exposed, so one is there.
    pub(crate) fn take_cqe(&self) -> cqueue::Entry {
        let mut state = self.lock();
        let entry = state
            .cq
            .pop_front()
            .expect("a completion the CQ tail exposed is posted");
        state.cq_head = state.cq_head.wrapping_add(1);
        entry
    }

    pub(crate) fn cq_overflowed(&self) -> bool {
        !self.lock().overflow.is_empty()
    }

    /// Completions dropped for a full CQ, which only a kernel without
    /// `IORING_FEAT_NODROP` does.
    pub(crate) fn dropped(&self) -> u32 {
        self.lock().dropped
    }

    pub(crate) fn eventfd_disabled(&self) -> bool {
        self.cq_flags.load(Ordering::SeqCst) & CQ_EVENTFD_DISABLED != 0
    }

    /// Set or clear `IORING_CQ_EVENTFD_DISABLED`, which a kernel before 5.8
    /// has no flag word for: there the eventfd is always signalled.
    pub(crate) fn set_eventfd_disabled(&self, disabled: bool) {
        if !self.kernel.has(since::CQ_EVENTFD_DISABLED) {
            return;
        }
        if disabled {
            self.cq_flags.fetch_or(CQ_EVENTFD_DISABLED, Ordering::SeqCst);
        } else {
            self.cq_flags.fetch_and(!CQ_EVENTFD_DISABLED, Ordering::SeqCst);
        }
    }

    /// Post a completion, in the kernel's order: store the tail, wake
    /// waiters, then load the flags and signal the eventfd if allowed.
    /// `inline` is a completion posted during the enter that submitted its
    /// op, which an eventfd registered async is not signalled for.
    fn post(&self, user_data: u64, result: i32, inline: bool) {
        let (added, eventfd) = {
            let mut state = self.lock();
            let mut added = state.push_cqe(
                cqueue::Entry {
                    user_data,
                    result,
                    flags: 0,
                },
                self.cq_entries,
            );
            added += state.complete_counted_timeouts(self.cq_entries);
            (
                added,
                state
                    .eventfd
                    .as_ref()
                    .filter(|eventfd| !(eventfd.async_only && inline))
                    .map(|eventfd| (eventfd.fd, Arc::clone(&eventfd.readiness))),
            )
        };
        self.publish(added);
        self.readiness.signal();
        if let Some((fd, readiness)) = eventfd
            && self.cq_flags.load(Ordering::SeqCst) & CQ_EVENTFD_DISABLED == 0
        {
            add_to_eventfd(fd);
            readiness.signal();
        }
    }

    /// Move held-back completions into the CQ as room allows, as every enter
    /// does.
    fn flush_overflow(&self) {
        let moved = {
            let mut state = self.lock();
            let mut moved = 0;
            while (state.cq.len() as u32) < self.cq_entries {
                let Some(entry) = state.overflow.pop_front() else {
                    break;
                };
                state.cq.push_back(entry);
                moved += 1;
            }
            moved
        };
        self.publish(moved);
    }

    /// Store the tail past `added` new completions, then wake the enters
    /// waiting for them.
    fn publish(&self, added: u32) {
        if added == 0 {
            return;
        }
        self.cq_tail.fetch_add(added, Ordering::SeqCst);
        // Each waiter drops its own entry when its enter returns, since a
        // timed one holds whether the kernel expired it.
        let wakers: Vec<Waker> = {
            let mut state = self.lock();
            state.published = state.published.wrapping_add(added);
            state.waiters.iter().map(|waiter| waiter.waker.clone()).collect()
        };
        for waker in wakers {
            waker.wake();
        }
    }

    // ---- enter ----

    /// `io_uring_enter`: submit up to `to_submit` published entries, then,
    /// with `getevents`, block until `min_complete` completions are visible.
    /// A bounded wait (`timeout`) may time out at any point Shuttle chooses,
    /// and does at once if nothing in flight could complete it.
    pub(crate) fn enter(
        &self,
        to_submit: u32,
        min_complete: u32,
        getevents: bool,
        timeout: Option<Timespec>,
    ) -> io::Result<usize> {
        self.flush_overflow();
        let mut submitted = 0;
        if to_submit > 0 {
            if self.cq_overflowed() {
                // The kernel refuses new work while it cannot flush the CQ.
                return Err(io::Error::from_raw_os_error(libc::EBUSY));
            }
            submitted = self.submit(to_submit);
        }
        if getevents && min_complete > 0 {
            let waited = self.wait(min_complete, timeout);
            // The kernel reports the entries it submitted, if it submitted
            // any, whatever became of the wait (`io_uring_enter` returns
            // `submitted ? submitted : ret`): a consumer counting submissions
            // by the return value never loses one to a timeout or a signal.
            if submitted == 0 {
                waited?;
            }
        }
        Ok(submitted)
    }

    fn submit(&self, to_submit: u32) -> usize {
        let entries: Vec<squeue::Entry> = {
            let mut state = self.lock();
            let n = (to_submit as usize).min(state.sq.len());
            state.sq.drain(..n).collect()
        };
        let n = entries.len();
        for entry in entries {
            self.submit_one(entry);
        }
        let kernel = {
            let mut state = self.lock();
            if state.inflight.is_empty() {
                None
            } else {
                state.kernel.take()
            }
        };
        if let Some(kernel) = kernel {
            kernel.wake();
        }
        n
    }

    fn submit_one(&self, entry: squeue::Entry) {
        let user_data = entry.user_data;
        if entry.flags.intersects(Flags::UNSUPPORTED) {
            self.post(user_data, -libc::EINVAL, true);
            return;
        }
        // An opcode newer than the kernel is one it does not know.
        if opcode::since(entry.op.code()).is_none_or(|release| !self.kernel.has(release)) {
            self.post(user_data, -libc::EINVAL, true);
            return;
        }
        // Before `IORING_FEAT_RW_CUR_POS` (5.6), offset -1 is no position.
        if entry.op.rw_offset() == Some(CUR_POS) && !self.kernel.has(since::RW_CUR_POS) {
            self.post(user_data, -libc::EINVAL, true);
            return;
        }
        let op = match entry.op {
            SqOp::Nop => Op::Nop,
            SqOp::Read {
                fd,
                buf,
                len,
                offset,
                rw_flags,
            } => match self.resolve(fd) {
                Ok(fd) => Op::Read {
                    fd,
                    buf,
                    len,
                    offset,
                    rw_flags,
                },
                Err(errno) => return self.post(user_data, -errno, true),
            },
            SqOp::Write {
                fd,
                buf,
                len,
                offset,
                rw_flags,
            } => match self.resolve(fd) {
                Ok(fd) => Op::Write {
                    fd,
                    buf,
                    len,
                    offset,
                    rw_flags,
                },
                Err(errno) => return self.post(user_data, -errno, true),
            },
            SqOp::Readv {
                fd,
                iovec,
                len,
                offset,
                rw_flags,
            } => match self.resolve(fd).and_then(|fd| Ok((fd, import_iovecs(iovec, len)?))) {
                Ok((fd, iovecs)) => Op::Readv {
                    fd,
                    iovecs,
                    offset,
                    rw_flags,
                    source: (!self.kernel.has(since::SUBMIT_STABLE)).then_some((iovec, len)),
                },
                Err(errno) => return self.post(user_data, -errno, true),
            },
            SqOp::Writev {
                fd,
                iovec,
                len,
                offset,
                rw_flags,
            } => match self.resolve(fd).and_then(|fd| Ok((fd, import_iovecs(iovec, len)?))) {
                Ok((fd, iovecs)) => Op::Writev {
                    fd,
                    iovecs,
                    offset,
                    rw_flags,
                    source: (!self.kernel.has(since::SUBMIT_STABLE)).then_some((iovec, len)),
                },
                Err(errno) => return self.post(user_data, -errno, true),
            },
            SqOp::Fsync { fd, datasync } => match self.resolve(fd) {
                Ok(fd) => Op::Fsync { fd, datasync },
                Err(errno) => return self.post(user_data, -errno, true),
            },
            SqOp::Fallocate { fd, offset, len, mode } => match self.resolve(fd) {
                Ok(fd) => Op::Fallocate { fd, offset, len, mode },
                Err(errno) => return self.post(user_data, -errno, true),
            },
            SqOp::Timeout { count } => Op::Timeout {
                count,
                since: self.lock().posted,
            },
            SqOp::AsyncCancel { user_data: target } => return self.cancel(user_data, target),
        };
        let refusable = matches!(
            op,
            Op::Read { .. } | Op::Write { .. } | Op::Readv { .. } | Op::Writev { .. }
        ) && !entry.flags.contains(Flags::ASYNC);
        let inline = !matches!(op, Op::Timeout { .. })
            && !entry.flags.contains(Flags::ASYNC)
            && thread_rng().gen_bool(INLINE_COMPLETION);
        let watch = op.watch(user_data);
        let op = InFlight {
            user_data,
            op,
            refusable,
            watch,
        };
        if inline {
            let result = op.complete();
            self.post(user_data, result, true);
        } else {
            self.lock().inflight.push(op);
        }
    }

    /// The fd an SQE's target names: the kernel looks a fixed file up when
    /// the op is submitted.
    fn resolve(&self, target: Target) -> Result<RawFd, i32> {
        match target {
            Target::Fd(fd) => Ok(fd),
            Target::Fixed(idx) => match &self.lock().files {
                Some(files) => match files.get(idx as usize) {
                    Some(&fd) if fd >= 0 => Ok(fd),
                    _ => Err(libc::EBADF),
                },
                None => Err(libc::EBADF),
            },
        }
    }

    /// `IORING_OP_ASYNC_CANCEL`. An op the kernel already handed to the
    /// device may be past cancelling, so a cancel of an in-flight op either
    /// cancels it or reports `-EALREADY` and lets it complete; Shuttle
    /// chooses. A cancel that finds nothing reports `-ENOENT`.
    fn cancel(&self, cancel_user_data: u64, target: u64) {
        let cancelled = {
            let mut state = self.lock();
            match state.inflight.iter().position(|op| op.user_data == target) {
                Some(i) if thread_rng().gen_bool(0.5) => {
                    state.inflight.swap_remove(i);
                    Some(true)
                }
                Some(_) => Some(false),
                None => None,
            }
        };
        match cancelled {
            Some(true) => {
                self.post(target, -libc::ECANCELED, true);
                self.post(cancel_user_data, 0, true);
            }
            Some(false) => self.post(cancel_user_data, -libc::EALREADY, true),
            None => self.post(cancel_user_data, -libc::ENOENT, true),
        }
    }

    /// Block the calling task until `min_complete` completions are posted.
    ///
    /// A timed wait is one more timer among the ops in flight: the kernel
    /// expires it at the rate it fires a timeout SQE, so a wait with I/O in
    /// flight times out only as a device that outlasts the timeout would. With
    /// nothing in flight to complete it, the timeout is all that is left and
    /// fires at once.
    fn wait(&self, min_complete: u32, timeout: Option<Timespec>) -> io::Result<()> {
        let timed = timeout.is_some();
        let id = {
            let mut state = self.lock();
            state.next_waiter_id += 1;
            state.next_waiter_id
        };
        // Whether the wait has yet to block: a signal can interrupt it only
        // then, and is drawn once per wait.
        let mut first_block = true;
        let result = future::block_on(poll_fn(|cx| {
            let mut state = self.lock();
            let visible = state.published.wrapping_sub(state.cq_head);
            if visible >= min_complete {
                return Poll::Ready(Ok(()));
            }
            // A wait cut short by a signal or its timeout still succeeds if
            // any completion is visible: `io_cqring_wait` returns 0 whenever
            // the CQ holds one, however few the caller asked for.
            let cut_short = |errno| {
                Poll::Ready(if visible > 0 {
                    Ok(())
                } else {
                    Err(io::Error::from_raw_os_error(errno))
                })
            };
            if mem::take(&mut first_block) && faults::interrupt() {
                return cut_short(libc::EINTR);
            }
            if timed {
                let expired = state.waiters.iter().any(|waiter| waiter.id == id && waiter.expired);
                if expired || state.inflight.is_empty() {
                    return cut_short(libc::ETIME);
                }
            }
            match state.waiters.iter_mut().find(|waiter| waiter.id == id) {
                Some(waiter) => waiter.waker.clone_from(cx.waker()),
                None => state.waiters.push(Waiter {
                    id,
                    waker: cx.waker().clone(),
                    timed,
                    expired: false,
                }),
            }
            Poll::Pending
        }));
        self.lock().waiters.retain(|waiter| waiter.id != id);
        result
    }

    // ---- registration ----

    pub(crate) fn register_files(&self, fds: Vec<RawFd>) -> io::Result<()> {
        let mut state = self.lock();
        if state.files.is_some() {
            return Err(io::Error::from_raw_os_error(libc::EBUSY));
        }
        state.files = Some(fds);
        Ok(())
    }

    pub(crate) fn register_files_update(&self, offset: u32, fds: &[RawFd]) -> io::Result<usize> {
        let mut state = self.lock();
        let Some(files) = state.files.as_mut() else {
            return Err(io::Error::from_raw_os_error(libc::ENXIO));
        };
        let start = offset as usize;
        let Some(slots) = files.get_mut(start..start + fds.len()) else {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        };
        slots.copy_from_slice(fds);
        Ok(fds.len())
    }

    pub(crate) fn unregister_files(&self) -> io::Result<()> {
        match self.lock().files.take() {
            Some(_) => Ok(()),
            None => Err(io::Error::from_raw_os_error(libc::ENXIO)),
        }
    }

    /// Register `fd` to be signalled as completions post; with `async_only`,
    /// only those that did not post inline (`IORING_REGISTER_EVENTFD_ASYNC`).
    pub(crate) fn register_eventfd(&self, fd: RawFd, async_only: bool) -> io::Result<()> {
        let readiness = Arc::new(Readiness::new());
        {
            let mut state = self.lock();
            if state.eventfd.is_some() {
                return Err(io::Error::from_raw_os_error(libc::EBUSY));
            }
            state.eventfd = Some(Eventfd {
                fd,
                readiness: Arc::clone(&readiness),
                async_only,
            });
        }
        // Outside the ring's lock: nothing that can switch tasks runs under it.
        async_fd::register(fd, readiness);
        Ok(())
    }

    pub(crate) fn unregister_eventfd(&self) -> io::Result<()> {
        let eventfd = self.lock().eventfd.take();
        match eventfd {
            Some(eventfd) => {
                async_fd::unregister(eventfd.fd, &eventfd.readiness);
                Ok(())
            }
            None => Err(io::Error::from_raw_os_error(libc::ENXIO)),
        }
    }

    // ---- close ----

    /// Close the ring, as dropping the last `IoUring` does.
    ///
    /// The kernel keeps working on ops in flight when a ring closes, reading
    /// and writing their buffers until they finish, so dropping a ring with
    /// ops in flight is only sound if their buffers outlive them. That is a
    /// teardown bug often enough that the simulation panics on it, unless the
    /// thread is already panicking or a timer has fired with I/O outstanding:
    /// an owner that saw the device hang may give up on the ring and leak the
    /// buffers, as a driver may once its drain timer expires. Either way it
    /// drops the ops rather than run them against buffers that may be gone.
    pub(crate) fn close(&self) {
        let (inflight, hung, kernel, eventfd) = {
            let mut state = self.lock();
            state.closed = true;
            let inflight = state.inflight.len();
            state.inflight.clear();
            (inflight, state.hung, state.kernel.take(), state.eventfd.take())
        };
        if let Some(kernel) = kernel {
            kernel.wake();
        }
        async_fd::unregister(self.fd, &self.readiness);
        if let Some(eventfd) = eventfd {
            async_fd::unregister(eventfd.fd, &eventfd.readiness);
        }
        if inflight > 0 && !hung && !thread::panicking() {
            panic!(
                "shuttle-io-uring: ring {} dropped with {inflight} op(s) in flight; the kernel \
                 would keep reading and writing their buffers after the ring is gone",
                self.fd
            );
        }
    }
}

impl State {
    /// Post one completion into the CQ, or hold it back if the CQ is full.
    /// Returns how many entries reached the CQ.
    fn push_cqe(&mut self, entry: cqueue::Entry, cq_entries: u32) -> u32 {
        self.posted += 1;
        if self.overflow.is_empty() && (self.cq.len() as u32) < cq_entries {
            self.cq.push_back(entry);
            1
        } else if self.nodrop {
            self.overflow.push_back(entry);
            0
        } else {
            // Before `IORING_FEAT_NODROP` (5.5) the kernel drops it, counted
            // in the CQ's overflow word, and nothing ever posts it.
            self.dropped = self.dropped.wrapping_add(1);
            0
        }
    }

    /// Whether an op other than a timer is in flight.
    fn io_in_flight(&self) -> bool {
        self.inflight.iter().any(|op| !matches!(op.op, Op::Timeout { .. }))
    }

    /// The timed enters the kernel may expire, in the order it counts them,
    /// after the ops in flight.
    fn wait_timers(&self) -> Vec<u64> {
        self.waiters
            .iter()
            .filter(|waiter| waiter.timed && !waiter.expired)
            .map(|waiter| waiter.id)
            .collect()
    }

    /// Candidates of one kind, in the order the kernel counts them: the ops
    /// in flight, then, among the timers, the timed enters.
    fn candidates(&self, timer: bool, waits: &[u64]) -> Vec<Pick> {
        let mut picks: Vec<Pick> = self
            .inflight
            .iter()
            .enumerate()
            .filter(|(_, op)| matches!(op.op, Op::Timeout { .. }) == timer)
            .map(|(i, _)| Pick::Op(i))
            .collect();
        if timer {
            picks.extend(waits.iter().copied().map(Pick::WaitTimer));
        }
        picks
    }

    /// What to deal with next, chosen by Shuttle: any of them when they are
    /// all I/O or all timers, else I/O, but for one time in
    /// [`faults::Faults::hang_odds`] a
    /// timer. A timer is a timeout SQE or a timed enter.
    fn pick_next(&self) -> Pick {
        let waits = self.wait_timers();
        let ops = self.inflight.len();
        let n = ops + waits.len();
        let timers = self
            .inflight
            .iter()
            .filter(|op| matches!(op.op, Op::Timeout { .. }))
            .count()
            + waits.len();
        if n == 1 {
            return if ops == 1 {
                Pick::Op(0)
            } else {
                Pick::WaitTimer(waits[0])
            };
        }
        let mut rng = thread_rng();
        if timers == 0 || timers == n {
            let k = rng.gen_range(0..n);
            return if k < ops {
                Pick::Op(k)
            } else {
                Pick::WaitTimer(waits[k - ops])
            };
        }
        let want_timer = rng.gen_ratio(1, faults::hang_odds());
        let k = rng.gen_range(0..if want_timer { timers } else { n - timers });
        let mut picks = self.candidates(want_timer, &waits);
        assert!(k < picks.len(), "k counts candidates of the chosen kind");
        picks.swap_remove(k)
    }

    /// Complete every timeout whose completion count is now met, with `0`.
    /// Each counts toward the others, as the kernel's CQ sequence does.
    fn complete_counted_timeouts(&mut self, cq_entries: u32) -> u32 {
        let mut added = 0;
        loop {
            let posted = self.posted;
            let Some(i) = self.inflight.iter().position(|op| match op.op {
                Op::Timeout { count, since } => count > 0 && posted - since >= u64::from(count),
                _ => false,
            }) else {
                return added;
            };
            let op = self.inflight.swap_remove(i);
            added += self.push_cqe(
                cqueue::Entry {
                    user_data: op.user_data,
                    result: 0,
                    flags: 0,
                },
                cq_entries,
            );
        }
    }
}
