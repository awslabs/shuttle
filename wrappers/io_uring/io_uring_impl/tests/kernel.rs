//! The simulated kernel's semantics, each checked across Shuttle schedules.

#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use shuttle::{check_random, future};
use shuttle_io_uring_impl::buffers;
use shuttle_io_uring_impl::faults::{self, Errors, Faults};
use shuttle_io_uring_impl::types::{Fd, Fixed, SubmitArgs, Timespec};
use shuttle_io_uring_impl::version::{self, Kernel};
use shuttle_io_uring_impl::{AsyncFd, IoUring, Probe, cqueue, opcode, squeue};

const ITERATIONS: usize = 300;

/// Enough iterations for a one-in-256 simulated hang to show up.
const HANG_ITERATIONS: usize = 5_000;

fn file() -> File {
    tempfile::tempfile().expect("temp file")
}

/// An `iovec` naming `buf`, for a vectored write.
fn iov(buf: &[u8]) -> libc::iovec {
    libc::iovec {
        iov_base: buf.as_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
    }
}

/// An `iovec` naming `buf`, for a vectored read.
fn iov_mut(buf: &mut [u8]) -> libc::iovec {
    libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    }
}

fn eventfd() -> OwnedFd {
    // SAFETY: plain syscall; the fd it returns is owned by nothing else.
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    assert!(fd >= 0, "eventfd: {}", std::io::Error::last_os_error());
    // SAFETY: `fd` was just created.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

fn push(ring: &IoUring, entry: squeue::Entry) {
    // SAFETY: each test keeps its buffers alive until their completions are
    // reaped, and pushes from one task at a time.
    unsafe { ring.submission_shared().push(&entry) }.expect("SQ has room");
}

fn reap(ring: &IoUring) -> Vec<cqueue::Entry> {
    // SAFETY: each test reaps from one task at a time.
    unsafe { ring.completion_shared() }.collect()
}

/// Reap until `n` completions have been collected.
fn reap_n(ring: &IoUring, n: usize) -> Vec<cqueue::Entry> {
    let mut cqes = Vec::new();
    while cqes.len() < n {
        ring.submit_and_wait(1).expect("enter");
        cqes.extend(reap(ring));
    }
    cqes
}

#[test]
fn write_then_read_round_trips() {
    check_random(
        || {
            let file = file();
            let fd = Fd(file.as_raw_fd());
            let ring = IoUring::new(8).unwrap();
            let data = b"hello from the simulated kernel".to_vec();
            push(
                &ring,
                opcode::Write::new(fd, data.as_ptr(), data.len() as u32)
                    .build()
                    .user_data(1),
            );
            let cqes = reap_n(&ring, 1);
            assert_eq!((cqes[0].user_data(), cqes[0].result()), (1, data.len() as i32));

            let mut buf = vec![0u8; data.len()];
            push(
                &ring,
                opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
                    .build()
                    .user_data(2),
            );
            let cqes = reap_n(&ring, 1);
            assert_eq!((cqes[0].user_data(), cqes[0].result()), (2, data.len() as i32));
            assert_eq!(buf, data);
        },
        ITERATIONS,
    );
}

#[test]
fn completions_arrive_in_any_order() {
    let orders = Arc::new(Mutex::new(HashSet::new()));
    let seen = Arc::clone(&orders);
    check_random(
        move || {
            let ring = IoUring::new(8).unwrap();
            for ud in 1..=3 {
                push(&ring, opcode::Nop::new().build().user_data(ud));
            }
            ring.submit().unwrap();
            let order: Vec<u64> = reap_n(&ring, 3).iter().map(|c| c.user_data()).collect();
            seen.lock().unwrap().insert(order);
        },
        ITERATIONS,
    );
    let orders = orders.lock().unwrap();
    assert_eq!(orders.len(), 6, "every order of three completions: {orders:?}");
}

#[test]
fn ops_complete_inline_or_stay_in_flight() {
    let outcomes = Arc::new(Mutex::new(HashSet::new()));
    let seen = Arc::clone(&outcomes);
    check_random(
        move || {
            let ring = IoUring::new(8).unwrap();
            push(&ring, opcode::Nop::new().build().user_data(1));
            ring.submit().unwrap();
            // Visible straight after the submit if it completed inline, or if
            // the kernel ran before the reap's tail load; in flight otherwise.
            let visible = reap(&ring).len();
            seen.lock().unwrap().insert(visible);
            if visible == 0 {
                reap_n(&ring, 1);
            }
        },
        ITERATIONS,
    );
    assert_eq!(*outcomes.lock().unwrap(), HashSet::from([0, 1]));
}

#[test]
fn eventfd_wakes_a_waiter_on_completion() {
    check_random(
        || {
            let ring = IoUring::new(8).unwrap();
            let efd = eventfd();
            ring.submitter().register_eventfd(efd.as_raw_fd()).unwrap();
            let async_fd = AsyncFd::new(efd).unwrap();
            let waiter = future::spawn(async move {
                let mut guard = async_fd.readable().await.unwrap();
                guard.clear_ready();
            });
            push(&ring, opcode::Nop::new().build().user_data(1));
            ring.submit().unwrap();
            future::block_on(waiter).unwrap();
            // The signal follows the post, so the completion is visible.
            assert_eq!(reap(&ring).len(), 1);
        },
        ITERATIONS,
    );
}

#[test]
fn eventfd_disabled_posts_without_signalling() {
    check_random(
        || {
            let ring = IoUring::new(8).unwrap();
            let efd = eventfd();
            ring.submitter().register_eventfd(efd.as_raw_fd()).unwrap();
            let async_fd = AsyncFd::new(efd).unwrap();
            // SAFETY: the only CQ handle.
            unsafe { ring.completion_shared() }.disable_eventfd();
            push(&ring, opcode::Nop::new().build().user_data(1));
            ring.submit_and_wait(1).unwrap();
            let mut cx = Context::from_waker(Waker::noop());
            assert!(async_fd.poll_read_ready(&mut cx).is_pending());
            assert_eq!(reap(&ring).len(), 1);
        },
        ITERATIONS,
    );
}

#[test]
fn clear_ready_keeps_a_newer_event() {
    check_random(
        || {
            let ring = IoUring::new(8).unwrap();
            let efd = eventfd();
            ring.submitter().register_eventfd(efd.as_raw_fd()).unwrap();
            let async_fd = AsyncFd::new(efd).unwrap();
            push(&ring, opcode::Nop::new().build().user_data(1));
            ring.submit().unwrap();
            let mut guard = future::block_on(async_fd.readable()).unwrap();
            push(&ring, opcode::Nop::new().build().user_data(2));
            ring.submit().unwrap();
            reap_n(&ring, 2);
            // Whether the second signal came before or after this clear, the
            // fd must read ready again: a clear never erases a newer event.
            guard.clear_ready();
            future::block_on(async_fd.readable()).unwrap();
        },
        ITERATIONS,
    );
}

#[test]
fn fixed_files_resolve_through_the_table() {
    check_random(
        || {
            let file = file();
            let ring = IoUring::new(8).unwrap();
            let submitter = ring.submitter();
            submitter.register_files_sparse(2).unwrap();
            assert_eq!(submitter.register_files_update(0, &[file.as_raw_fd()]).unwrap(), 1);
            let data = [7u8; 16];
            for (ud, idx) in [(1, 0), (2, 1), (3, 5)] {
                push(
                    &ring,
                    opcode::Write::new(Fixed(idx), data.as_ptr(), data.len() as u32)
                        .build()
                        .user_data(ud),
                );
            }
            ring.submit().unwrap();
            let mut results: Vec<(u64, i32)> = reap_n(&ring, 3).iter().map(|c| (c.user_data(), c.result())).collect();
            results.sort();
            assert_eq!(results, vec![(1, 16), (2, -libc::EBADF), (3, -libc::EBADF)]);
        },
        ITERATIONS,
    );
}

#[test]
fn unsynced_entries_are_not_submitted() {
    check_random(
        || {
            let ring = IoUring::new(8).unwrap();
            // SAFETY: the only SQ handle.
            let mut sq = unsafe { ring.submission_shared() };
            // SAFETY: a nop has no buffers.
            unsafe { sq.push(&opcode::Nop::new().build().user_data(1)) }.unwrap();
            assert_eq!(ring.submit().unwrap(), 0, "pushed but not synced");
            sq.sync();
            assert_eq!(ring.submit().unwrap(), 1);
            drop(sq);
            reap_n(&ring, 1);
        },
        ITERATIONS,
    );
}

#[test]
fn bounded_wait_with_nothing_in_flight_times_out() {
    check_random(
        || {
            let ring = IoUring::new(8).unwrap();
            let timeout = Timespec::from(Duration::from_millis(1));
            let args = SubmitArgs::new().timespec(&timeout);
            let err = ring.submitter().submit_with_args(1, &args).unwrap_err();
            assert_eq!(err.raw_os_error(), Some(libc::ETIME));
        },
        ITERATIONS,
    );
}

#[test]
#[should_panic(expected = "deadlock")]
fn unbounded_wait_with_nothing_in_flight_deadlocks() {
    check_random(
        || {
            let ring = IoUring::new(8).unwrap();
            let _ = ring.submit_and_wait(1);
        },
        ITERATIONS,
    );
}

#[test]
fn timeout_fires_or_meets_its_count() {
    let outcomes = Arc::new(Mutex::new(HashSet::new()));
    let seen = Arc::clone(&outcomes);
    check_random(
        move || {
            let ring = IoUring::new(8).unwrap();
            let timeout = Timespec::from(Duration::from_secs(1));
            push(
                &ring,
                opcode::Timeout::new(&raw const timeout).count(2).build().user_data(9),
            );
            push(&ring, opcode::Nop::new().build().user_data(1));
            push(&ring, opcode::Nop::new().build().user_data(2));
            ring.submit().unwrap();
            let cqes = reap_n(&ring, 3);
            let timer = cqes.iter().find(|c| c.user_data() == 9).unwrap().result();
            assert!(timer == 0 || timer == -libc::ETIME, "timeout result {timer}");
            seen.lock().unwrap().insert(timer);
        },
        // The timer fires with I/O in flight one time in 256 per completion.
        HANG_ITERATIONS,
    );
    assert_eq!(*outcomes.lock().unwrap(), HashSet::from([0, -libc::ETIME]));
}

#[test]
fn cancel_cancels_or_reports_already() {
    let outcomes = Arc::new(Mutex::new(HashSet::new()));
    let seen = Arc::clone(&outcomes);
    check_random(
        move || {
            let ring = IoUring::new(8).unwrap();
            let timeout = Timespec::from(Duration::from_secs(1));
            push(&ring, opcode::Timeout::new(&raw const timeout).build().user_data(1));
            ring.submit().unwrap();
            // Let the timer fire first in some schedules.
            future::block_on(future::yield_now());
            push(&ring, opcode::AsyncCancel::new(1).build().user_data(2));
            ring.submit().unwrap();
            let mut results: Vec<(u64, i32)> = reap_n(&ring, 2).iter().map(|c| (c.user_data(), c.result())).collect();
            results.sort();
            let cancel = results[1].1;
            let target = results[0].1;
            match cancel {
                0 => assert_eq!(target, -libc::ECANCELED),
                c if c == -libc::EALREADY || c == -libc::ENOENT => {
                    assert_eq!(target, -libc::ETIME)
                }
                other => panic!("cancel result {other}"),
            }
            seen.lock().unwrap().insert(cancel);
        },
        ITERATIONS,
    );
    assert_eq!(
        *outcomes.lock().unwrap(),
        HashSet::from([0, -libc::EALREADY, -libc::ENOENT])
    );
}

#[test]
#[should_panic(expected = "in flight")]
fn dropping_a_ring_with_ops_in_flight_panics() {
    check_random(
        || {
            let ring = IoUring::new(8).unwrap();
            let timeout = Timespec::from(Duration::from_secs(1));
            push(&ring, opcode::Timeout::new(&raw const timeout).build().user_data(1));
            ring.submit().unwrap();
            drop(ring);
        },
        ITERATIONS,
    );
}

#[test]
fn a_ring_that_saw_a_hang_may_drop_ops_in_flight() {
    let hangs = Arc::new(Mutex::new(0usize));
    let seen = Arc::clone(&hangs);
    check_random(
        move || {
            let file = file();
            let ring = IoUring::new(8).unwrap();
            let mut buf = vec![0u8; 512];
            let timeout = Timespec::from(Duration::from_secs(60));
            push(&ring, opcode::Timeout::new(&raw const timeout).build().user_data(9));
            push(
                &ring,
                opcode::Read::new(Fd(file.as_raw_fd()), buf.as_mut_ptr(), buf.len() as u32)
                    .build()
                    .user_data(1),
            );
            ring.submit().unwrap();
            let cqes = reap_n(&ring, 1);
            let read_done = cqes.iter().any(|c| c.user_data() == 1);
            if cqes[0].user_data() == 9 && !read_done {
                // The timer outlasted the read: the device hung, and the
                // owner may give up on the ring with the read in flight.
                assert_eq!(cqes[0].result(), -libc::ETIME);
                *seen.lock().unwrap() += 1;
                drop(ring);
            } else if cqes.len() < 2 {
                reap_n(&ring, 1);
            }
        },
        HANG_ITERATIONS,
    );
    let hangs = *hangs.lock().unwrap();
    assert!(
        hangs > 0 && hangs < HANG_ITERATIONS / 20,
        "{hangs} of {HANG_ITERATIONS} iterations saw the timer outlast the read"
    );
}

#[test]
fn async_fd_rejects_an_unregistered_fd() {
    check_random(
        || {
            let efd = eventfd();
            let err = AsyncFd::new(efd).map(|_| ()).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        },
        10,
    );
}

#[test]
fn ring_fd_is_readable_after_a_completion() {
    check_random(
        || {
            let ring = Arc::new(IoUring::new(8).unwrap());
            struct RingFd(Arc<IoUring>);
            impl AsRawFd for RingFd {
                fn as_raw_fd(&self) -> std::os::fd::RawFd {
                    self.0.as_raw_fd()
                }
            }
            let async_fd = AsyncFd::new(RingFd(Arc::clone(&ring))).unwrap();
            let mut cx = Context::from_waker(Waker::noop());
            assert!(matches!(async_fd.poll_read_ready(&mut cx), Poll::Pending));
            push(&ring, opcode::Nop::new().build().user_data(1));
            ring.submit().unwrap();
            future::block_on(async_fd.readable()).unwrap();
            assert_eq!(reap(&ring).len(), 1);
        },
        ITERATIONS,
    );
}

/// With every refusable op refused, a read or write submitted without
/// `IOSQE_ASYNC` reports `-EAGAIN` having moved at most a block-aligned part
/// of its range, and one submitted with it completes.
#[test]
fn refused_ops_report_eagain_after_moving_a_part() {
    const BLOCK: usize = 4096;
    check_random(
        || {
            faults::set_faults(Faults {
                eagain: 1.0,
                ..Faults::default()
            });
            let file = file();
            let fd = Fd(file.as_raw_fd());
            let ring = IoUring::new(8).unwrap();
            let data = vec![0xab_u8; 3 * BLOCK];
            push(
                &ring,
                opcode::Write::new(fd, data.as_ptr(), data.len() as u32)
                    .build()
                    .user_data(1),
            );
            let cqes = reap_n(&ring, 1);
            assert_eq!(cqes[0].result(), -libc::EAGAIN);
            let moved = file.metadata().unwrap().len() as usize;
            assert!(moved.is_multiple_of(BLOCK) && moved < data.len(), "moved {moved}");

            push(
                &ring,
                opcode::Write::new(fd, data.as_ptr(), data.len() as u32)
                    .build()
                    .flags(squeue::Flags::ASYNC)
                    .user_data(2),
            );
            let cqes = reap_n(&ring, 1);
            assert_eq!(cqes[0].result(), data.len() as i32);

            let mut buf = vec![0_u8; data.len()];
            push(
                &ring,
                opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
                    .build()
                    .user_data(3),
            );
            let cqes = reap_n(&ring, 1);
            assert_eq!(cqes[0].result(), -libc::EAGAIN);
            let filled = buf.iter().take_while(|&&b| b == 0xab).count();
            assert!(filled.is_multiple_of(BLOCK) && filled < buf.len(), "filled {filled}");
            assert!(buf[filled..].iter().all(|&b| b == 0), "only a prefix is filled");
            faults::set_faults(Faults::default());
        },
        ITERATIONS,
    );
}

/// With errors on, a read fails with `-EIO` or returns a block-aligned part
/// of its range short, and a `fallocate` fails with `-ENOSPC`; with them off
/// again the same ops complete in full.
#[test]
fn posted_errors_surface_and_stop_when_turned_off() {
    const BLOCK: usize = 4096;
    check_random(
        || {
            let file = file();
            let fd = Fd(file.as_raw_fd());
            let ring = IoUring::new(8).unwrap();
            let data = vec![0xab_u8; 3 * BLOCK];
            push(
                &ring,
                opcode::Write::new(fd, data.as_ptr(), data.len() as u32)
                    .build()
                    .flags(squeue::Flags::ASYNC)
                    .user_data(1),
            );
            assert_eq!(reap_n(&ring, 1)[0].result(), data.len() as i32);

            let read = |user_data| {
                let mut buf = vec![0_u8; data.len()];
                push(
                    &ring,
                    opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
                        .build()
                        .flags(squeue::Flags::ASYNC)
                        .user_data(user_data),
                );
                reap_n(&ring, 1)[0].result()
            };
            faults::set_errors(Errors {
                eio: 1.0,
                ..Errors::default()
            });
            assert_eq!(read(2), -libc::EIO);
            faults::set_errors(Errors {
                short_read: 1.0,
                ..Errors::default()
            });
            let short = read(3);
            assert!(
                short == data.len() as i32
                    || (short > 0 && (short as usize).is_multiple_of(BLOCK) && (short as usize) < data.len()),
                "a short read returned {short}"
            );
            faults::set_errors(Errors {
                enospc: 1.0,
                ..Errors::default()
            });
            push(
                &ring,
                opcode::Fallocate::new(fd, BLOCK as u64)
                    .offset(data.len() as u64)
                    .build()
                    .user_data(4),
            );
            assert_eq!(reap_n(&ring, 1)[0].result(), -libc::ENOSPC);
            let posted = faults::take_posted();
            assert_eq!((posted.eio, posted.enospc), (1, 1));

            faults::set_errors(Errors::default());
            assert_eq!(read(5), data.len() as i32);
        },
        ITERATIONS,
    );
}

/// Vectored ops gather from and scatter into their buffers in order, at their
/// offset.
#[test]
fn vectored_write_then_read_round_trips() {
    check_random(
        || {
            let file = file();
            let fd = Fd(file.as_raw_fd());
            let ring = IoUring::new(8).unwrap();
            let parts: [&[u8]; 3] = [b"hello ", b"from the ", b"simulated kernel"];
            let data = parts.concat();
            let writes: Vec<libc::iovec> = parts.iter().map(|part| iov(part)).collect();
            push(
                &ring,
                opcode::Writev::new(fd, writes.as_ptr(), writes.len() as u32)
                    .build()
                    .user_data(1),
            );
            let cqes = reap_n(&ring, 1);
            assert_eq!((cqes[0].user_data(), cqes[0].result()), (1, data.len() as i32));

            // Split the read elsewhere than the write, and start it past 0.
            let mut head = vec![0_u8; 4];
            let mut tail = vec![0_u8; data.len() - 2 - head.len()];
            let reads = [iov_mut(&mut head), iov_mut(&mut tail)];
            push(
                &ring,
                opcode::Readv::new(fd, reads.as_ptr(), reads.len() as u32)
                    .offset(2)
                    .build()
                    .user_data(2),
            );
            let cqes = reap_n(&ring, 1);
            assert_eq!((cqes[0].user_data(), cqes[0].result()), (2, data.len() as i32 - 2));
            assert_eq!([head, tail].concat(), data[2..]);
        },
        ITERATIONS,
    );
}

/// The kernel copies a vectored op's `iovec` array at submission, so the
/// submitter may reuse the array once the submit returns, with the op still in
/// flight.
#[test]
fn vectored_iovec_array_is_copied_at_submission() {
    check_random(
        || {
            let file = file();
            let fd = Fd(file.as_raw_fd());
            let ring = IoUring::new(8).unwrap();
            let data = vec![0xaa_u8; 64];
            let other = vec![0xbb_u8; 64];
            let mut writes = [iov(&data)];
            push(
                &ring,
                opcode::Writev::new(fd, writes.as_ptr(), writes.len() as u32)
                    .build()
                    .user_data(1),
            );
            ring.submit().unwrap();
            // A kernel that read the array at completion would write `other`.
            writes[0] = iov(&other);
            std::hint::black_box(&writes);
            assert_eq!(reap_n(&ring, 1)[0].result(), data.len() as i32);
            let mut written = vec![0_u8; data.len()];
            file.read_exact_at(&mut written, 0).unwrap();
            assert_eq!(written, data);
        },
        ITERATIONS,
    );
}

/// A vectored op with more than `UIO_MAXIOV` entries fails with `-EINVAL`, and
/// one with a null array with `-EFAULT`, as the kernel's import of the array
/// does; an empty array moves nothing.
#[test]
fn vectored_ops_check_their_iovec_arrays() {
    check_random(
        || {
            let file = file();
            let fd = Fd(file.as_raw_fd());
            let ring = IoUring::new(8).unwrap();
            let mut byte = [0_u8; 1];
            let too_many = vec![iov_mut(&mut byte); libc::UIO_MAXIOV as usize + 1];
            push(
                &ring,
                opcode::Readv::new(fd, too_many.as_ptr(), too_many.len() as u32)
                    .build()
                    .user_data(1),
            );
            push(&ring, opcode::Writev::new(fd, std::ptr::null(), 1).build().user_data(2));
            push(&ring, opcode::Writev::new(fd, std::ptr::null(), 0).build().user_data(3));
            ring.submit().unwrap();
            let mut results: Vec<(u64, i32)> = reap_n(&ring, 3).iter().map(|c| (c.user_data(), c.result())).collect();
            results.sort();
            assert_eq!(results, vec![(1, -libc::EINVAL), (2, -libc::EFAULT), (3, 0)]);
        },
        ITERATIONS,
    );
}

/// Vectored ops name fixed files through the registered table, as
/// single-buffer ones do.
#[test]
fn vectored_ops_resolve_fixed_files() {
    check_random(
        || {
            let file = file();
            let ring = IoUring::new(8).unwrap();
            let submitter = ring.submitter();
            submitter.register_files_sparse(2).unwrap();
            submitter.register_files_update(0, &[file.as_raw_fd()]).unwrap();
            let data = [7_u8; 16];
            let writes = [iov(&data)];
            for (ud, idx) in [(1, 0), (2, 1)] {
                push(
                    &ring,
                    opcode::Writev::new(Fixed(idx), writes.as_ptr(), writes.len() as u32)
                        .build()
                        .user_data(ud),
                );
            }
            ring.submit().unwrap();
            let mut results: Vec<(u64, i32)> = reap_n(&ring, 2).iter().map(|c| (c.user_data(), c.result())).collect();
            results.sort();
            assert_eq!(results, vec![(1, 16), (2, -libc::EBADF)]);

            let mut buf = [0_u8; 16];
            let reads = [iov_mut(&mut buf)];
            push(
                &ring,
                opcode::Readv::new(Fixed(0), reads.as_ptr(), reads.len() as u32)
                    .build()
                    .user_data(3),
            );
            assert_eq!(reap_n(&ring, 1)[0].result(), 16);
            assert_eq!(buf, data);
        },
        ITERATIONS,
    );
}

/// A refused vectored op, and a failed or short vectored read, moves a
/// block-aligned prefix of its range across the buffers it lists, as a
/// single-buffer op does.
#[test]
fn vectored_faults_move_a_block_aligned_prefix() {
    const BLOCK: usize = 4096;
    check_random(
        || {
            let file = file();
            let fd = Fd(file.as_raw_fd());
            let ring = IoUring::new(8).unwrap();
            // Buffer boundaries fall inside blocks, so a prefix splits a buffer.
            let bufs = [
                vec![0x11_u8; BLOCK + 512],
                vec![0x22_u8; BLOCK],
                vec![0x33_u8; 2 * BLOCK - 512],
            ];
            let data = bufs.concat();
            let writes: Vec<libc::iovec> = bufs.iter().map(|buf| iov(buf)).collect();
            let write = |user_data, flags| {
                push(
                    &ring,
                    opcode::Writev::new(fd, writes.as_ptr(), writes.len() as u32)
                        .build()
                        .flags(flags)
                        .user_data(user_data),
                );
                reap_n(&ring, 1)[0].result()
            };
            // Scatters into buffers split elsewhere than the write's.
            let read = |user_data, flags| {
                let mut head = vec![0_u8; 3 * BLOCK - 100];
                let mut tail = vec![0_u8; data.len() - head.len()];
                let reads = [iov_mut(&mut head), iov_mut(&mut tail)];
                push(
                    &ring,
                    opcode::Readv::new(fd, reads.as_ptr(), reads.len() as u32)
                        .build()
                        .flags(flags)
                        .user_data(user_data),
                );
                let result = reap_n(&ring, 1)[0].result();
                (result, [head, tail].concat())
            };
            // How much of `buf` holds a prefix of `data`; the rest is unread.
            let filled = |buf: &[u8]| {
                let n = buf.iter().zip(&data).take_while(|(b, d)| b == d).count();
                assert!(buf[n..].iter().all(|&b| b == 0), "only a prefix is filled");
                n
            };

            faults::set_faults(Faults {
                eagain: 1.0,
                ..Faults::default()
            });
            assert_eq!(write(1, squeue::Flags::empty()), -libc::EAGAIN);
            let len = file.metadata().unwrap().len() as usize;
            assert!(len.is_multiple_of(BLOCK) && len < data.len(), "moved {len}");
            let mut moved = vec![0_u8; len];
            file.read_exact_at(&mut moved, 0).unwrap();
            assert_eq!(moved, data[..len], "a refused write moves a prefix");
            assert_eq!(write(2, squeue::Flags::ASYNC), data.len() as i32);
            let (result, buf) = read(3, squeue::Flags::empty());
            assert_eq!(result, -libc::EAGAIN);
            let n = filled(&buf);
            assert!(n.is_multiple_of(BLOCK) && n < data.len(), "a refused read filled {n}");
            faults::set_faults(Faults::default());

            faults::set_errors(Errors {
                eio: 1.0,
                ..Errors::default()
            });
            let (result, buf) = read(4, squeue::Flags::ASYNC);
            assert_eq!(result, -libc::EIO);
            let n = filled(&buf);
            assert!(n.is_multiple_of(BLOCK) && n < data.len(), "a failed read filled {n}");
            faults::set_errors(Errors {
                short_read: 1.0,
                ..Errors::default()
            });
            let (short, buf) = read(5, squeue::Flags::ASYNC);
            let short = usize::try_from(short).expect("a short read succeeds");
            assert!(
                short == data.len() || (short > 0 && short.is_multiple_of(BLOCK) && short < data.len()),
                "a short read returned {short}"
            );
            assert_eq!(filled(&buf), short);
            faults::set_errors(Errors::default());
            assert_eq!(read(6, squeue::Flags::ASYNC), (data.len() as i32, data.clone()));
        },
        ITERATIONS,
    );
}

/// The kernel reads a write's buffer when the op completes, so changing the
/// buffer in flight is caught at the op that did it.
#[test]
#[should_panic(expected = "changed while it was in flight")]
fn a_write_buffer_changed_in_flight_panics() {
    check_random(
        || {
            let file = file();
            let ring = IoUring::new(8).unwrap();
            let mut data = vec![0xaa_u8; 64];
            let ptr = data.as_mut_ptr();
            push(
                &ring,
                opcode::Write::new(Fd(file.as_raw_fd()), ptr, data.len() as u32)
                    .build()
                    .flags(squeue::Flags::ASYNC)
                    .user_data(1),
            );
            ring.submit().unwrap();
            // SAFETY: the buffer outlives the op; writing it here is the
            // breach of the ring's contract the check is for.
            unsafe { ptr.write(0xbb) };
            reap_n(&ring, 1);
        },
        ITERATIONS,
    );
}

/// The kernel owns a read's buffer until its completion is reaped, so writing
/// the buffer in flight is caught.
#[test]
#[should_panic(expected = "was written while it was in flight")]
fn a_read_buffer_written_in_flight_panics() {
    check_random(
        || {
            let file = file();
            let ring = IoUring::new(8).unwrap();
            let mut buf = vec![0_u8; 64];
            let ptr = buf.as_mut_ptr();
            push(
                &ring,
                opcode::Read::new(Fd(file.as_raw_fd()), ptr, buf.len() as u32)
                    .build()
                    .flags(squeue::Flags::ASYNC)
                    .user_data(1),
            );
            ring.submit().unwrap();
            // SAFETY: as above.
            unsafe { ptr.write(0xcc) };
            reap_n(&ring, 1);
        },
        ITERATIONS,
    );
}

/// A read's buffer holds poison while the kernel owns it, so a consumer that
/// reads it before reaping the completion gets neither the buffer's own bytes
/// nor the file's. The bytes past the ones read are the buffer's own.
#[test]
fn a_read_buffer_holds_poison_while_it_is_in_flight() {
    const OWN: u8 = 0x5a;
    let outcomes = Arc::new(Mutex::new(HashSet::new()));
    let seen = Arc::clone(&outcomes);
    check_random(
        move || {
            let file = file();
            let data = vec![0xd7_u8; 32];
            file.write_all_at(&data, 0).unwrap();
            let ring = IoUring::new(8).unwrap();
            // Twice the file's bytes, so the read is short and the tail must
            // come back as the buffer's own.
            let mut buf = vec![OWN; 2 * data.len()];
            push(
                &ring,
                opcode::Read::new(Fd(file.as_raw_fd()), buf.as_mut_ptr(), buf.len() as u32)
                    .build()
                    .flags(squeue::Flags::ASYNC)
                    .user_data(1),
            );
            ring.submit().unwrap();
            let peek = buf.clone();
            assert_eq!(reap_n(&ring, 1)[0].result(), data.len() as i32);
            assert_eq!(&buf[..data.len()], data, "the file's bytes are read");
            assert!(
                buf[data.len()..].iter().all(|&b| b == OWN),
                "the bytes past the read are the buffer's own"
            );
            let in_flight = peek != buf;
            if in_flight {
                assert!(
                    peek.iter().any(|&b| b != OWN),
                    "a buffer in flight holds poison, not its own bytes"
                );
            }
            seen.lock().unwrap().insert(in_flight);
        },
        ITERATIONS,
    );
    assert!(
        outcomes.lock().unwrap().contains(&true),
        "no iteration read the buffer while the op was in flight"
    );
}

/// Two ops in flight may share a buffer when both write it, and may not when
/// either reads it.
#[test]
fn two_writes_may_share_a_buffer() {
    check_random(
        || {
            let files = [file(), file()];
            let ring = IoUring::new(8).unwrap();
            let data = [0xee_u8; 64];
            for (ud, file) in files.iter().enumerate() {
                push(
                    &ring,
                    opcode::Write::new(Fd(file.as_raw_fd()), data.as_ptr(), data.len() as u32)
                        .build()
                        .flags(squeue::Flags::ASYNC)
                        .user_data(ud as u64),
                );
            }
            ring.submit().unwrap();
            for cqe in reap_n(&ring, 2) {
                assert_eq!(cqe.result(), data.len() as i32);
            }
        },
        ITERATIONS,
    );
}

#[test]
#[should_panic(expected = "overlaps")]
fn a_read_sharing_an_in_flight_buffer_panics() {
    check_random(
        || {
            let file = file();
            let fd = Fd(file.as_raw_fd());
            let ring = IoUring::new(8).unwrap();
            let mut buf = vec![0_u8; 64];
            let ptr = buf.as_mut_ptr();
            push(
                &ring,
                opcode::Write::new(fd, ptr, buf.len() as u32)
                    .build()
                    .flags(squeue::Flags::ASYNC)
                    .user_data(1),
            );
            // The second half of the same buffer: overlapping, not identical.
            push(
                &ring,
                opcode::Read::new(fd, unsafe { ptr.add(32) }, 32)
                    .build()
                    .flags(squeue::Flags::ASYNC)
                    .user_data(2),
            );
            ring.submit().unwrap();
            reap_n(&ring, 2);
        },
        ITERATIONS,
    );
}

/// Two ops in flight over the same bytes of one file are caught only with
/// [`buffers::Checks::file_ranges`] on, since the ring allows them.
#[test]
fn overlapping_file_ranges_are_caught_only_when_asked() {
    let two_writes = |ring: &IoUring, fd: Fd, first: &[u8], second: &[u8]| {
        push(
            ring,
            opcode::Write::new(fd, first.as_ptr(), first.len() as u32)
                .build()
                .flags(squeue::Flags::ASYNC)
                .user_data(1),
        );
        push(
            ring,
            opcode::Write::new(fd, second.as_ptr(), second.len() as u32)
                .offset(16)
                .build()
                .flags(squeue::Flags::ASYNC)
                .user_data(2),
        );
        ring.submit().unwrap();
    };
    check_random(
        move || {
            let file = file();
            let fd = Fd(file.as_raw_fd());
            let first = vec![0x11_u8; 64];
            let second = vec![0x22_u8; 64];

            // Off by default: both land, in whichever order the kernel chose.
            let ring = IoUring::new(8).unwrap();
            two_writes(&ring, fd, &first, &second);
            for cqe in reap_n(&ring, 2) {
                assert_eq!(cqe.result(), 64);
            }
            drop(ring);

            buffers::set_checks(buffers::Checks {
                file_ranges: true,
                ..buffers::Checks::default()
            });
            let ring = IoUring::new(8).unwrap();
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                two_writes(&ring, fd, &first, &second);
            }));
            buffers::set_checks(buffers::Checks::default());
            let message = *caught
                .expect_err("overlapping writes to one file are caught")
                .downcast::<String>()
                .expect("the check panics with a message");
            assert!(
                message.contains("covers bytes 16..80") && message.contains("0..64"),
                "{message}"
            );
            // The second write never reached the kernel, so only the first is
            // in flight.
            reap_n(&ring, 1);
        },
        ITERATIONS,
    );
}

/// With the checks off, the simulation leaves the buffers alone: a write
/// changed in flight lands whichever bytes it holds when the op runs.
#[test]
fn the_checks_can_be_turned_off() {
    check_random(
        || {
            buffers::set_checks(buffers::Checks {
                writes: false,
                reads: false,
                overlap: false,
                file_ranges: false,
            });
            let file = file();
            let ring = IoUring::new(8).unwrap();
            let mut data = vec![0xaa_u8; 64];
            let ptr = data.as_mut_ptr();
            push(
                &ring,
                opcode::Write::new(Fd(file.as_raw_fd()), ptr, data.len() as u32)
                    .build()
                    .flags(squeue::Flags::ASYNC)
                    .user_data(1),
            );
            ring.submit().unwrap();
            // SAFETY: the buffer outlives the op.
            unsafe { ptr.write(0xbb) };
            assert_eq!(reap_n(&ring, 1)[0].result(), data.len() as i32);
            let mut written = vec![0_u8; data.len()];
            file.read_exact_at(&mut written, 0).unwrap();
            assert_eq!(written[0], 0xbb, "the kernel reads the buffer at completion");
            buffers::set_checks(buffers::Checks::default());
        },
        ITERATIONS,
    );
}

/// Read an eventfd's counter, or `None` when it holds nothing.
fn read_eventfd(fd: &OwnedFd) -> Option<u64> {
    let mut value: u64 = 0;
    // SAFETY: a plain read of eight bytes from an eventfd this test owns,
    // opened non-blocking, so an empty counter returns `EAGAIN` at once.
    let read = unsafe { libc::read(fd.as_raw_fd(), (&raw mut value).cast(), 8) };
    if read == 8 {
        Some(value)
    } else {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EAGAIN),
            "an eventfd read fails only on an empty counter"
        );
        None
    }
}

/// A timed enter with I/O in flight times out as rarely as a timeout SQE
/// fires: the device outlasting the timeout is a hang, not a coin flip. The
/// ring may then be dropped with the op in flight, as a consumer that gave up
/// on the device does.
#[test]
fn a_timed_wait_with_io_in_flight_rarely_times_out() {
    let timeouts = Arc::new(Mutex::new(0usize));
    let seen = Arc::clone(&timeouts);
    check_random(
        move || {
            let file = file();
            let ring = IoUring::new(8).unwrap();
            let mut buf = vec![0_u8; 64];
            push(
                &ring,
                opcode::Read::new(Fd(file.as_raw_fd()), buf.as_mut_ptr(), buf.len() as u32)
                    .build()
                    .flags(squeue::Flags::ASYNC)
                    .user_data(1),
            );
            // Submitted on its own: an enter that submits reports the count,
            // whatever became of its wait.
            ring.submit().unwrap();
            let ts = Timespec::from(Duration::from_millis(100));
            let args = SubmitArgs::new().timespec(&ts);
            match ring.submitter().submit_with_args(1, &args) {
                Ok(_) => {
                    assert_eq!(reap_n(&ring, 1)[0].user_data(), 1);
                }
                Err(e) => {
                    assert_eq!(e.raw_os_error(), Some(libc::ETIME));
                    *seen.lock().unwrap() += 1;
                    // The wait timed out with the read in flight, so the
                    // device hung: giving up on the ring is allowed.
                    drop(ring);
                }
            }
        },
        HANG_ITERATIONS,
    );
    let timeouts = *timeouts.lock().unwrap();
    assert!(
        timeouts > 0 && timeouts < HANG_ITERATIONS / 20,
        "{timeouts} of {HANG_ITERATIONS} timed waits timed out with the read in flight"
    );
}

/// A timed enter waiting on completions that cannot come times out at once,
/// even with a timer in flight: nothing is left but the timeouts.
#[test]
fn a_timed_wait_times_out_at_once_behind_a_timer() {
    check_random(
        || {
            let ring = IoUring::new(8).unwrap();
            let timeout = Timespec::from(Duration::from_secs(60));
            push(&ring, opcode::Timeout::new(&raw const timeout).build().user_data(9));
            ring.submit().unwrap();
            // Two completions, of which only the timer's can come.
            let ts = Timespec::from(Duration::from_millis(100));
            let args = SubmitArgs::new().timespec(&ts);
            // Either the wait's timeout fires first, or the timer's completion
            // posts first and the wait returns with it visible, as the kernel
            // returns 0 from a timed-out wait whenever the CQ holds a
            // completion.
            match ring.submitter().submit_with_args(2, &args) {
                Ok(0) => assert_eq!(reap(&ring).len(), 1, "the timer's completion is visible"),
                Ok(n) => panic!("nothing to submit, yet {n} submitted"),
                Err(e) => {
                    assert_eq!(e.raw_os_error(), Some(libc::ETIME));
                    reap_n(&ring, 1);
                }
            }
        },
        ITERATIONS,
    );
}

/// Read an eventfd's counter until it has given up `want`, letting the kernel
/// task run in between: a completion posts before the kernel signals the
/// eventfd, so a consumer can see the completion with the counter still short.
fn drain_eventfd(fd: &OwnedFd, want: u64) -> u64 {
    let mut total = 0;
    for _ in 0..100 {
        total += read_eventfd(fd).unwrap_or(0);
        if total >= want {
            break;
        }
        shuttle::thread::yield_now();
    }
    total
}

/// Each signalled completion adds one to the registered eventfd's counter, so
/// a consumer that sleeps on the fd and reads it to reset it sees them.
#[test]
fn the_eventfd_counts_signalled_completions() {
    check_random(
        || {
            let ring = IoUring::new(8).unwrap();
            let efd = eventfd();
            ring.submitter().register_eventfd(efd.as_raw_fd()).unwrap();
            for ud in 1..=3 {
                push(&ring, opcode::Nop::new().build().user_data(ud));
            }
            ring.submit().unwrap();
            assert_eq!(reap_n(&ring, 3).len(), 3);
            assert_eq!(drain_eventfd(&efd, 3), 3, "every completion added one to the counter");
            assert_eq!(read_eventfd(&efd), None, "the reads reset the counter");

            // A completion posted with signalling off adds nothing.
            unsafe { ring.completion_shared() }.disable_eventfd();
            push(&ring, opcode::Nop::new().build().user_data(4));
            ring.submit().unwrap();
            assert_eq!(reap_n(&ring, 1).len(), 1);
            assert_eq!(drain_eventfd(&efd, 1), 0);

            // Nor does one posted after the fd is unregistered.
            unsafe { ring.completion_shared() }.enable_eventfd();
            ring.submitter().unregister_eventfd().unwrap();
            push(&ring, opcode::Nop::new().build().user_data(5));
            ring.submit().unwrap();
            assert_eq!(reap_n(&ring, 1).len(), 1);
            assert_eq!(drain_eventfd(&efd, 1), 0);
        },
        ITERATIONS,
    );
}

/// An enter that submits reports how many entries it submitted, whatever
/// became of its wait, as `io_uring_enter` returns `submitted ? submitted :
/// ret`; only an enter that submitted nothing surfaces the wait's error.
#[test]
fn an_enter_that_submits_reports_the_count_whatever_its_wait_did() {
    check_random(
        || {
            let ring = IoUring::new(8).unwrap();
            let timeout = Timespec::from(Duration::from_secs(60));
            push(&ring, opcode::Timeout::new(&raw const timeout).build().user_data(9));
            // Two completions wanted, and only the timer's can come, so the
            // wait times out; the enter still submitted the timer.
            let ts = Timespec::from(Duration::from_millis(100));
            let args = SubmitArgs::new().timespec(&ts);
            assert_eq!(ring.submitter().submit_with_args(2, &args).unwrap(), 1);
            reap_n(&ring, 1);
            // With nothing to submit, nothing in flight and nothing visible,
            // the wait's timeout surfaces.
            let error = ring
                .submitter()
                .submit_with_args(1, &args)
                .expect_err("nothing submitted, so the timeout surfaces");
            assert_eq!(error.raw_os_error(), Some(libc::ETIME));
        },
        ITERATIONS,
    );
}

/// A signal can interrupt an enter that blocks for completions. The kernel
/// reports it only from an enter that submitted nothing, so an enter that
/// submits and then waits never sees it.
#[test]
fn an_interrupted_wait_surfaces_only_from_an_enter_that_submitted_nothing() {
    let interrupted = Arc::new(Mutex::new(0usize));
    let seen = Arc::clone(&interrupted);
    check_random(
        move || {
            let file = file();
            let fd = Fd(file.as_raw_fd());
            let ring = IoUring::new(8).unwrap();
            let mut first = [0_u8; 64];
            let mut second = [0_u8; 64];
            faults::take_posted();
            faults::set_errors(Errors {
                eintr: 1.0,
                ..Errors::default()
            });
            push(
                &ring,
                opcode::Read::new(fd, first.as_mut_ptr(), first.len() as u32)
                    .build()
                    .flags(squeue::Flags::ASYNC)
                    .user_data(1),
            );
            // Submits, so reports the count even if its wait was interrupted.
            assert_eq!(ring.submit_and_wait(1).unwrap(), 1);
            push(
                &ring,
                opcode::Read::new(fd, second.as_mut_ptr(), second.len() as u32)
                    .offset(64)
                    .build()
                    .flags(squeue::Flags::ASYNC)
                    .user_data(2),
            );
            ring.submit().unwrap();
            // Submits nothing: an interrupted wait surfaces, unless a
            // completion is visible, which the kernel returns instead.
            match ring.submit_and_wait(2) {
                Ok(0) => {}
                Ok(n) => panic!("an enter with nothing to submit submitted {n}"),
                Err(e) => {
                    assert_eq!(e.raw_os_error(), Some(libc::EINTR));
                    *seen.lock().unwrap() += 1;
                }
            }
            faults::set_errors(Errors::default());
            let mut reaped = 0;
            while reaped < 2 {
                reaped += reap(&ring).len();
                if reaped < 2 {
                    ring.submit_and_wait(1).unwrap();
                }
            }
            assert!(
                faults::take_posted().eintr >= 1,
                "the first wait blocked or not, the second did"
            );
        },
        ITERATIONS,
    );
    assert!(*interrupted.lock().unwrap() > 0, "no wait was interrupted");
}

/// [`Faults::hang_odds`] sets how often a timer beats I/O: at 1, every choice
/// between them goes to the timer, so a timed wait with I/O in flight times out
/// whenever its waiter runs before the read posts, about half the time, where
/// at the default odds it almost never does.
#[test]
fn hang_odds_set_how_often_a_timer_beats_io() {
    let timeouts = Arc::new(Mutex::new(0usize));
    let seen = Arc::clone(&timeouts);
    check_random(
        move || {
            faults::set_faults(Faults {
                hang_odds: 1,
                ..Faults::default()
            });
            let file = file();
            let ring = IoUring::new(8).unwrap();
            let mut buf = vec![0_u8; 64];
            push(
                &ring,
                opcode::Read::new(Fd(file.as_raw_fd()), buf.as_mut_ptr(), buf.len() as u32)
                    .build()
                    .flags(squeue::Flags::ASYNC)
                    .user_data(1),
            );
            ring.submit().unwrap();
            let ts = Timespec::from(Duration::from_millis(100));
            let args = SubmitArgs::new().timespec(&ts);
            let timed_out = match ring.submitter().submit_with_args(1, &args) {
                Ok(_) => false,
                Err(e) => {
                    assert_eq!(e.raw_os_error(), Some(libc::ETIME));
                    true
                }
            };
            faults::set_faults(Faults::default());
            if timed_out {
                *seen.lock().unwrap() += 1;
                // The device hung, so the ring may go with the read in flight.
                drop(ring);
            } else {
                reap_n(&ring, 1);
            }
        },
        ITERATIONS,
    );
    let timeouts = *timeouts.lock().unwrap();
    assert!(
        timeouts > ITERATIONS / 4,
        "{timeouts} of {ITERATIONS} timed waits timed out with every choice a hang"
    );
}

/// An eventfd registered async is signalled only for completions that did not
/// post inline: an op refused at submit counts for a plain eventfd, not for an
/// async one.
#[test]
fn an_async_eventfd_skips_completions_posted_inline() {
    check_random(
        || {
            for async_only in [false, true] {
                let ring = IoUring::new(8).unwrap();
                let efd = eventfd();
                let submitter = ring.submitter();
                if async_only {
                    submitter.register_eventfd_async(efd.as_raw_fd()).unwrap();
                } else {
                    submitter.register_eventfd(efd.as_raw_fd()).unwrap();
                }
                let data = [7_u8; 16];
                // Never inline: the kernel task completes it.
                push(
                    &ring,
                    opcode::Nop::new().build().flags(squeue::Flags::ASYNC).user_data(1),
                );
                // Always inline: refused at submit, with no file table.
                push(
                    &ring,
                    opcode::Write::new(Fixed(5), data.as_ptr(), data.len() as u32)
                        .build()
                        .user_data(2),
                );
                ring.submit().unwrap();
                assert_eq!(reap_n(&ring, 2).len(), 2);
                let want = if async_only { 1 } else { 2 };
                assert_eq!(drain_eventfd(&efd, want), want, "async_only {async_only}");
                assert_eq!(read_eventfd(&efd), None);
            }
        },
        ITERATIONS,
    );
}

/// An eventfd's counter as an `io::Result`, `WouldBlock` when it is empty, as
/// a `try_io` closure reads it.
fn read_eventfd_io(fd: &OwnedFd) -> std::io::Result<u64> {
    read_eventfd(fd).ok_or_else(|| std::io::ErrorKind::WouldBlock.into())
}

/// `try_io` runs the I/O a readiness was waited for, and on `WouldBlock`
/// clears the readiness, so the next wait waits for a new event.
#[test]
fn try_io_clears_readiness_when_the_io_would_block() {
    check_random(
        || {
            let ring = IoUring::new(8).unwrap();
            let efd = eventfd();
            ring.submitter().register_eventfd(efd.as_raw_fd()).unwrap();
            let async_fd = AsyncFd::new(efd).unwrap();
            push(&ring, opcode::Nop::new().build().user_data(1));
            ring.submit().unwrap();
            let count = future::block_on(async {
                let mut guard = async_fd.readable().await.unwrap();
                let count = guard
                    .try_io(|fd| read_eventfd_io(fd.get_ref()))
                    .expect("the signal left a count to read")
                    .expect("the read succeeds");
                assert!(
                    guard.try_io(|fd| read_eventfd_io(fd.get_ref())).is_err(),
                    "read out, the same readiness now finds nothing"
                );
                count
            });
            assert_eq!(count, 1);
            let mut cx = Context::from_waker(Waker::noop());
            assert!(
                async_fd.poll_read_ready(&mut cx).is_pending(),
                "cleared until the next completion signals"
            );
            assert_eq!(reap_n(&ring, 1).len(), 1);
        },
        ITERATIONS,
    );
}

/// Per-op `RWF_*` flags reach the system call: `RWF_APPEND` appends whatever
/// the offset, as `pwritev2(2)` does, and `RWF_DSYNC` is accepted.
#[test]
fn rw_flags_reach_the_system_call() {
    check_random(
        || {
            let file = file();
            let fd = Fd(file.as_raw_fd());
            let ring = IoUring::new(8).unwrap();
            let first = *b"abc";
            let second = *b"def";
            let third = *b"ghi";
            push(
                &ring,
                opcode::Write::new(fd, first.as_ptr(), 3)
                    .rw_flags(libc::RWF_DSYNC)
                    .build()
                    .user_data(1),
            );
            assert_eq!(reap_n(&ring, 1)[0].result(), 3);
            push(
                &ring,
                opcode::Write::new(fd, second.as_ptr(), 3)
                    .rw_flags(libc::RWF_APPEND)
                    .build()
                    .user_data(2),
            );
            assert_eq!(reap_n(&ring, 1)[0].result(), 3);
            let writes = [iov(&third)];
            push(
                &ring,
                opcode::Writev::new(fd, writes.as_ptr(), 1)
                    .rw_flags(libc::RWF_APPEND)
                    .build()
                    .user_data(3),
            );
            assert_eq!(reap_n(&ring, 1)[0].result(), 3);
            let mut written = [0_u8; 9];
            file.read_exact_at(&mut written, 0).unwrap();
            assert_eq!(&written, b"abcdefghi", "appended, though written at offset 0");
        },
        ITERATIONS,
    );
}

/// The file position for an op at offset −1, used and advanced
/// (`IORING_FEAT_RW_CUR_POS`).
fn file_position(fd: &File) -> i64 {
    // SAFETY: a plain lseek on a file this test owns.
    unsafe { libc::lseek(fd.as_raw_fd(), 0, libc::SEEK_CUR) }
}

/// An op at offset −1 uses the file's position and advances it, as the
/// kernel does with `IORING_FEAT_RW_CUR_POS`.
#[test]
fn offset_minus_one_uses_and_advances_the_file_position() {
    check_random(
        || {
            let file = file();
            let fd = Fd(file.as_raw_fd());
            let ring = IoUring::new(8).unwrap();
            assert!(ring.params().is_feature_rw_cur_pos());
            let hello = *b"hello";
            push(
                &ring,
                opcode::Write::new(fd, hello.as_ptr(), 5)
                    .offset(u64::MAX)
                    .build()
                    .user_data(1),
            );
            assert_eq!(reap_n(&ring, 1)[0].result(), 5);
            let world = *b"world";
            let writes = [iov(&world)];
            push(
                &ring,
                opcode::Writev::new(fd, writes.as_ptr(), 1)
                    .offset(u64::MAX)
                    .build()
                    .user_data(2),
            );
            assert_eq!(reap_n(&ring, 1)[0].result(), 5);
            assert_eq!(file_position(&file), 10);

            // SAFETY: as above.
            unsafe { libc::lseek(file.as_raw_fd(), 0, libc::SEEK_SET) };
            let mut head = [0_u8; 5];
            push(
                &ring,
                opcode::Read::new(fd, head.as_mut_ptr(), 5)
                    .offset(u64::MAX)
                    .build()
                    .user_data(3),
            );
            assert_eq!(reap_n(&ring, 1)[0].result(), 5);
            let mut tail = [0_u8; 5];
            let reads = [iov_mut(&mut tail)];
            push(
                &ring,
                opcode::Readv::new(fd, reads.as_ptr(), 1)
                    .offset(u64::MAX)
                    .build()
                    .user_data(4),
            );
            assert_eq!(reap_n(&ring, 1)[0].result(), 5);
            assert_eq!((&head, &tail), (b"hello", b"world"));
            assert_eq!(file_position(&file), 10);
        },
        ITERATIONS,
    );
}

/// Run `f` with this thread's new rings behaving as `kernel`, then put the
/// default back, so a failing iteration cannot leak an old kernel into the
/// next test on the thread.
fn as_kernel<R>(kernel: Kernel, f: impl FnOnce() -> R) -> R {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            version::set_kernel(Kernel::Current);
        }
    }
    version::set_kernel(kernel);
    let _restore = Restore;
    f()
}

/// On Linux 5.4 what the kernel lacks is gone as the kernel shows it: newer
/// features unreported, newer opcodes `-EINVAL`, a timed enter and newer
/// registrations and setup flags `EINVAL`. The 5.1 opcodes still work.
#[test]
fn linux_5_4_lacks_what_it_lacks() {
    check_random(
        || {
            as_kernel(Kernel::Linux5_4, || {
                let file = file();
                let fd = Fd(file.as_raw_fd());
                let ring = IoUring::new(8).unwrap();
                let params = ring.params();
                assert!(params.is_feature_single_mmap());
                assert!(!params.is_feature_nodrop());
                assert!(!params.is_feature_submit_stable());
                assert!(!params.is_feature_rw_cur_pos());
                assert!(!params.is_feature_ext_arg());

                let data = [5_u8; 16];
                let writes = [iov(&data)];
                let einval = [
                    opcode::Write::new(fd, data.as_ptr(), 16).build().user_data(1),
                    opcode::Fallocate::new(fd, 4096).build().user_data(2),
                    opcode::AsyncCancel::new(7).build().user_data(3),
                    opcode::Writev::new(fd, writes.as_ptr(), 1)
                        .offset(u64::MAX)
                        .build()
                        .user_data(4),
                ];
                for entry in einval {
                    push(&ring, entry);
                }
                push(&ring, opcode::Writev::new(fd, writes.as_ptr(), 1).build().user_data(5));
                push(&ring, opcode::Fsync::new(fd).build().user_data(6));
                ring.submit().unwrap();
                let mut results: Vec<(u64, i32)> =
                    reap_n(&ring, 6).iter().map(|c| (c.user_data(), c.result())).collect();
                results.sort();
                assert_eq!(
                    results,
                    vec![
                        (1, -libc::EINVAL),
                        (2, -libc::EINVAL),
                        (3, -libc::EINVAL),
                        (4, -libc::EINVAL),
                        (5, 16),
                        (6, 0),
                    ]
                );

                let einval = |result: std::io::Result<()>| {
                    assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EINVAL));
                };
                let ts = Timespec::from(Duration::from_millis(100));
                let args = SubmitArgs::new().timespec(&ts);
                einval(ring.submitter().submit_with_args(1, &args).map(drop));
                einval(ring.submitter().register_probe(&mut Probe::new()));
                einval(ring.submitter().register_eventfd_async(eventfd().as_raw_fd()));
                einval(ring.submitter().register_files_sparse(2));
                einval(
                    IoUring::<squeue::Entry, cqueue::Entry>::builder()
                        .setup_single_issuer()
                        .build(8)
                        .map(drop),
                );
                einval(
                    IoUring::<squeue::Entry, cqueue::Entry>::builder()
                        .setup_cqsize(64)
                        .build(8)
                        .map(drop),
                );
            });
        },
        ITERATIONS,
    );
}

/// Without `IORING_FEAT_NODROP`, as before 5.5, a completion that finds the CQ
/// full is dropped and counted in `overflow()`, and nothing posts it. On a
/// current kernel the same completion is held back and posts later.
#[test]
fn linux_5_4_drops_completions_that_find_the_cq_full() {
    let drops = Arc::new(Mutex::new(0usize));
    let seen = Arc::clone(&drops);
    check_random(
        move || {
            for kernel in [Kernel::Current, Kernel::Linux5_4] {
                let (delivered, dropped) = as_kernel(kernel, || {
                    // One SQ entry, so two CQ entries, and three completions.
                    let ring = IoUring::new(1).unwrap();
                    for ud in 1..=3 {
                        push(
                            &ring,
                            opcode::Nop::new().build().flags(squeue::Flags::ASYNC).user_data(ud),
                        );
                        // A current kernel refuses new work with `EBUSY`
                        // while it holds a completion back.
                        while let Err(e) = ring.submit() {
                            assert_eq!(e.raw_os_error(), Some(libc::EBUSY));
                            assert_eq!(reap(&ring).len(), 2);
                        }
                    }
                    let overflow = || unsafe { ring.completion_shared() }.overflow() as usize;
                    let mut delivered = 0;
                    for _ in 0..100 {
                        delivered += reap(&ring).len();
                        if delivered + overflow() == 3 {
                            break;
                        }
                        // Lets the kernel post, and flushes a held-back one.
                        ring.submit().unwrap();
                        shuttle::thread::yield_now();
                    }
                    (delivered, overflow())
                });
                assert_eq!(delivered + dropped, 3, "{kernel:?}");
                match kernel {
                    Kernel::Current => assert_eq!(dropped, 0, "a current kernel drops nothing"),
                    _ => *seen.lock().unwrap() += dropped,
                }
            }
        },
        ITERATIONS,
    );
    assert!(*drops.lock().unwrap() > 0, "Linux 5.4 never dropped a completion");
}

/// Without `IORING_FEAT_SUBMIT_STABLE`, as before 5.5, the kernel reads a
/// vectored op's `iovec` array when it runs the op, so the array must outlive
/// the op, not just the submit: one changed in flight panics, and with the
/// buffer checks off, the op runs on the array as it now is.
#[test]
fn linux_5_4_reads_the_iovec_array_when_it_runs_the_op() {
    check_random(
        || {
            as_kernel(Kernel::Linux5_4, || {
                buffers::set_checks(buffers::Checks {
                    writes: false,
                    reads: false,
                    overlap: false,
                    file_ranges: false,
                });
                let file = file();
                let ring = IoUring::new(8).unwrap();
                let data = [0xaa_u8; 64];
                let other = [0xbb_u8; 64];
                let mut writes = [iov(&data)];
                push(
                    &ring,
                    opcode::Writev::new(Fd(file.as_raw_fd()), writes.as_ptr(), 1)
                        .build()
                        .flags(squeue::Flags::ASYNC)
                        .user_data(1),
                );
                ring.submit().unwrap();
                writes[0] = iov(&other);
                std::hint::black_box(&writes);
                assert_eq!(reap_n(&ring, 1)[0].result(), 64);
                buffers::set_checks(buffers::Checks::default());
                let mut written = [0_u8; 64];
                file.read_exact_at(&mut written, 0).unwrap();
                assert_eq!(written, other, "the array as it was when the op ran");
            });
        },
        ITERATIONS,
    );
}

#[test]
#[should_panic(expected = "iovec array changed while it was in flight")]
fn linux_5_4_panics_on_an_iovec_array_changed_in_flight() {
    check_random(
        || {
            as_kernel(Kernel::Linux5_4, || {
                let file = file();
                let ring = IoUring::new(8).unwrap();
                let data = [0xaa_u8; 64];
                let other = [0xbb_u8; 64];
                let mut writes = [iov(&data)];
                push(
                    &ring,
                    opcode::Writev::new(Fd(file.as_raw_fd()), writes.as_ptr(), 1)
                        .build()
                        .flags(squeue::Flags::ASYNC)
                        .user_data(1),
                );
                ring.submit().unwrap();
                writes[0] = iov(&other);
                std::hint::black_box(&writes);
                reap_n(&ring, 1);
            });
        },
        ITERATIONS,
    );
}

/// Before `IORING_CQ_EVENTFD_DISABLED` (5.8) the CQ has no flag word, so
/// signalling cannot be turned off: every completion signals the eventfd.
#[test]
fn linux_5_4_signals_the_eventfd_for_every_completion() {
    check_random(
        || {
            as_kernel(Kernel::Linux5_4, || {
                let ring = IoUring::new(8).unwrap();
                let efd = eventfd();
                ring.submitter().register_eventfd(efd.as_raw_fd()).unwrap();
                unsafe { ring.completion_shared() }.disable_eventfd();
                assert!(!unsafe { ring.completion_shared() }.eventfd_disabled());
                push(&ring, opcode::Nop::new().build().user_data(1));
                ring.submit().unwrap();
                assert_eq!(reap_n(&ring, 1).len(), 1);
                assert_eq!(drain_eventfd(&efd, 1), 1);
            });
        },
        ITERATIONS,
    );
}

/// A wait cut short by its timeout or a signal returns success if any
/// completion is visible, however few it asked for, as `io_uring_enter` does
/// (`io_cqring_wait` returns 0 whenever the CQ holds a completion).
#[test]
fn a_cut_short_wait_succeeds_with_a_completion_visible() {
    check_random(
        || {
            let ring = IoUring::new(8).unwrap();
            push(
                &ring,
                opcode::Nop::new().build().flags(squeue::Flags::ASYNC).user_data(1),
            );
            // Leaves the completion visible, unreaped.
            ring.submit_and_wait(1).unwrap();

            // Timed, wanting two, nothing in flight: it times out, with one
            // visible.
            let ts = Timespec::from(Duration::from_millis(100));
            let args = SubmitArgs::new().timespec(&ts);
            assert_eq!(ring.submitter().submit_with_args(2, &args).unwrap(), 0);

            // Untimed, wanting two: a signal cuts it short, with one visible.
            faults::set_errors(Errors {
                eintr: 1.0,
                ..Errors::default()
            });
            assert_eq!(ring.submit_and_wait(2).unwrap(), 0);
            faults::set_errors(Errors::default());
            assert_eq!(reap(&ring).len(), 1);
        },
        ITERATIONS,
    );
}
