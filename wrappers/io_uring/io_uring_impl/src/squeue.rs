//! Submission queue, mirroring [`io_uring::squeue`](https://docs.rs/io-uring/0.7/io_uring/squeue).

use std::fmt;
use std::mem;

use bitflags::bitflags;

use crate::kernel::Ring;
use crate::types::sealed::Target;

/// Selects the SQE shape. Only the default 64-byte [`Entry`] is simulated.
pub trait EntryMarker: private::Sealed + Clone + fmt::Debug + Send + Sync + 'static {}

/// One submission queue entry, built by an [`opcode`](crate::opcode) builder.
///
/// The simulation keeps the op in decoded form rather than the kernel's
/// layout; every method the real `Entry` offers is source-compatible.
#[derive(Clone, Debug)]
pub struct Entry {
    pub(crate) op: SqOp,
    pub(crate) user_data: u64,
    pub(crate) flags: Flags,
}

impl Entry {
    pub(crate) fn new(op: SqOp, fd: Option<Target>) -> Self {
        let flags = match fd {
            Some(Target::Fixed(_)) => Flags::FIXED_FILE,
            _ => Flags::empty(),
        };
        Self {
            op,
            user_data: 0,
            flags,
        }
    }

    /// Add `flags` to the entry's flags, as the real crate does (it ORs).
    pub fn flags(mut self, flags: Flags) -> Entry {
        self.flags |= flags;
        self
    }

    /// Clear the entry's flags.
    pub fn clear_flags(mut self) -> Entry {
        self.flags = Flags::empty();
        self
    }

    /// Set the `user_data` the completion carries back.
    pub fn user_data(mut self, user_data: u64) -> Entry {
        self.user_data = user_data;
        self
    }

    /// The `user_data` set on this entry.
    pub fn get_user_data(&self) -> u64 {
        self.user_data
    }

    /// Accepted for source compatibility; credentials are not simulated.
    pub fn personality(self, _personality: u16) -> Entry {
        self
    }
}

impl EntryMarker for Entry {}

bitflags! {
    /// Per-SQE flags, mirroring `IOSQE_*`.
    ///
    /// The simulation supports `FIXED_FILE` (set by the builders for
    /// [`Fixed`](crate::types::Fixed) targets) and `ASYNC` (the op never
    /// completes inline). The others post `-EINVAL`, so a consumer that
    /// needs links or drains learns at once that the simulation does not
    /// model them.
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Flags: u8 {
        const FIXED_FILE = 1 << 0;
        const IO_DRAIN = 1 << 1;
        const IO_LINK = 1 << 2;
        const IO_HARDLINK = 1 << 3;
        const ASYNC = 1 << 4;
        const BUFFER_SELECT = 1 << 5;
        const SKIP_SUCCESS = 1 << 6;
    }
}

impl Flags {
    /// The flags the simulation rejects with `-EINVAL`.
    pub(crate) const UNSUPPORTED: Flags = Flags::IO_DRAIN
        .union(Flags::IO_LINK)
        .union(Flags::IO_HARDLINK)
        .union(Flags::BUFFER_SELECT)
        .union(Flags::SKIP_SUCCESS);
}

/// The op an SQE asks for, decoded. Raw buffer pointers carry the real
/// contract: the buffer must stay valid until the op's completion is reaped.
#[derive(Clone, Debug)]
pub(crate) enum SqOp {
    Nop,
    Read {
        fd: Target,
        buf: *mut u8,
        len: u32,
        offset: u64,
        rw_flags: i32,
    },
    Write {
        fd: Target,
        buf: *const u8,
        len: u32,
        offset: u64,
        rw_flags: i32,
    },
    /// `len` counts `iovec` entries. The kernel reads the array when it
    /// consumes the entry, so only it must stay valid until the submit.
    Readv {
        fd: Target,
        iovec: *const libc::iovec,
        len: u32,
        offset: u64,
        rw_flags: i32,
    },
    Writev {
        fd: Target,
        iovec: *const libc::iovec,
        len: u32,
        offset: u64,
        rw_flags: i32,
    },
    Fsync {
        fd: Target,
        datasync: bool,
    },
    Fallocate {
        fd: Target,
        offset: u64,
        len: u64,
        mode: i32,
    },
    Timeout {
        count: u32,
    },
    AsyncCancel {
        user_data: u64,
    },
}

impl SqOp {
    /// The opcode, as the real crate's builder writes it.
    pub(crate) fn code(&self) -> u8 {
        use crate::opcode;
        match self {
            SqOp::Nop => opcode::Nop::CODE,
            SqOp::Read { .. } => opcode::Read::CODE,
            SqOp::Write { .. } => opcode::Write::CODE,
            SqOp::Readv { .. } => opcode::Readv::CODE,
            SqOp::Writev { .. } => opcode::Writev::CODE,
            SqOp::Fsync { .. } => opcode::Fsync::CODE,
            SqOp::Fallocate { .. } => opcode::Fallocate::CODE,
            SqOp::Timeout { .. } => opcode::Timeout::CODE,
            SqOp::AsyncCancel { .. } => opcode::AsyncCancel::CODE,
        }
    }

    /// The op's file offset, for a read or write.
    pub(crate) fn rw_offset(&self) -> Option<u64> {
        match self {
            SqOp::Read { offset, .. }
            | SqOp::Write { offset, .. }
            | SqOp::Readv { offset, .. }
            | SqOp::Writev { offset, .. } => Some(*offset),
            _ => None,
        }
    }
}

// SAFETY: the raw pointers are the caller's buffers under the io_uring
// contract: valid and unaliased until the op's CQE is reaped. The simulated
// kernel touches them only from the task that completes the op, while the
// ring's lock serializes access to the entry itself.
unsafe impl Send for SqOp {}
unsafe impl Sync for SqOp {}

/// The submission queue, mirroring the real crate's `SubmissionQueue`.
///
/// As on a real ring, pushed entries become visible to the kernel only when
/// the queue is [`sync`](Self::sync)ed or dropped: a consumer that pushes and
/// then submits through the [`Submitter`](crate::Submitter) while still
/// holding unsynced entries submits nothing, just as on Linux.
pub struct SubmissionQueue<'a, E: EntryMarker = Entry> {
    ring: &'a Ring,
    /// Entries pushed through this handle and not yet published.
    local: Vec<Entry>,
    _marker: std::marker::PhantomData<E>,
}

impl<'a, E: EntryMarker> SubmissionQueue<'a, E> {
    pub(crate) fn new(ring: &'a Ring) -> Self {
        Self {
            ring,
            local: Vec::new(),
            _marker: std::marker::PhantomData,
        }
    }

    /// Publish pushed entries to the kernel, as the real crate's `sync`
    /// stores the queue tail.
    pub fn sync(&mut self) {
        if !self.local.is_empty() {
            self.ring.publish_sqes(mem::take(&mut self.local));
        }
    }

    /// Entries queued and not yet consumed by the kernel, published or not.
    pub fn len(&self) -> usize {
        self.ring.sq_published() + self.local.len()
    }

    /// Whether nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the queue is at capacity.
    pub fn is_full(&self) -> bool {
        self.len() >= self.capacity()
    }

    /// The number of entries the queue holds.
    pub fn capacity(&self) -> usize {
        self.ring.sq_entries as usize
    }

    /// Always `false`: the simulation has no SQ polling thread to wake.
    pub fn need_wakeup(&self) -> bool {
        false
    }

    /// Whether completions overflowed the CQ and wait to be flushed by the
    /// next enter (`IORING_SQ_CQ_OVERFLOW`).
    pub fn cq_overflow(&self) -> bool {
        self.ring.cq_overflowed()
    }

    /// Always `false`: completions post as they happen, so there is no
    /// deferred task work to run (see the crate docs on `COOP_TASKRUN`).
    pub fn taskrun(&self) -> bool {
        false
    }

    /// Always zero: the simulation never drops an invalid SQE silently.
    pub fn dropped(&self) -> u32 {
        0
    }

    /// Push one entry.
    ///
    /// # Safety
    ///
    /// As in the real crate: the entry's buffers must stay valid until its
    /// completion is reaped, and this handle must be the queue's only writer.
    pub unsafe fn push(&mut self, entry: &E) -> Result<(), PushError> {
        if self.is_full() {
            return Err(PushError);
        }
        self.local.push(downcast(entry));
        Ok(())
    }

    /// Push several entries, all or none.
    ///
    /// # Safety
    ///
    /// As for [`push`](Self::push).
    pub unsafe fn push_multiple(&mut self, entries: &[E]) -> Result<(), PushError> {
        if self.capacity() - self.len() < entries.len() {
            return Err(PushError);
        }
        self.local.extend(entries.iter().map(downcast));
        Ok(())
    }
}

impl<E: EntryMarker> Drop for SubmissionQueue<'_, E> {
    /// The real handle stores its tail on drop, which publishes its entries.
    fn drop(&mut self) {
        self.sync();
    }
}

/// `E` is always [`Entry`]: the only [`EntryMarker`].
fn downcast<E: EntryMarker>(entry: &E) -> Entry {
    let any: &dyn std::any::Any = entry;
    any.downcast_ref::<Entry>()
        .expect("Entry is the only simulated SQE shape")
        .clone()
}

/// The queue was full, mirroring the real crate's `PushError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushError;

impl fmt::Display for PushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("submission queue is full")
    }
}

impl std::error::Error for PushError {}

mod private {
    pub trait Sealed {}
    impl Sealed for super::Entry {}
}
