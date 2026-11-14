//! Everything the simulation keeps belongs to one Shuttle execution, so no
//! execution sees what an earlier one did, and a failing execution replays on
//! its own as it ran in the test.

#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};

use shuttle::check_random;
use shuttle_io_uring_impl::buffers::{self, Checks};
use shuttle_io_uring_impl::faults::{self, Errors, Faults, Posted};
use shuttle_io_uring_impl::types::Fd;
use shuttle_io_uring_impl::version::{self, Kernel};
use shuttle_io_uring_impl::{AsyncFd, IoUring, cqueue, opcode, squeue};

const ITERATIONS: usize = 20;

fn file() -> File {
    tempfile::tempfile().expect("temp file")
}

fn push(ring: &IoUring, entry: squeue::Entry) {
    // SAFETY: each test keeps its buffers alive until their completions are
    // reaped, or leaks the ring, and pushes from one task at a time.
    unsafe { ring.submission_shared().push(&entry) }.expect("SQ has room");
}

/// Reap until `n` completions have been collected.
fn reap_n(ring: &IoUring, n: usize) -> Vec<cqueue::Entry> {
    let mut cqes = Vec::new();
    while cqes.len() < n {
        ring.submit_and_wait(1).expect("enter");
        // SAFETY: each test reaps from one task at a time.
        cqes.extend(unsafe { ring.completion_shared() });
    }
    cqes
}

/// A read of `len` bytes into `buf` that never completes inline.
fn read(file: &File, buf: *mut u8, len: u32) -> squeue::Entry {
    opcode::Read::new(Fd(file.as_raw_fd()), buf, len)
        .build()
        .flags(squeue::Flags::ASYNC)
        .user_data(1)
}

/// Each execution starts with the default faults, errors, buffer checks and
/// kernel, and with no errors counted, though every earlier one changed them
/// all and left them changed.
#[test]
fn settings_start_at_their_defaults_in_every_execution() {
    check_random(
        || {
            assert_eq!(faults::faults(), Faults::default());
            assert_eq!(faults::errors(), Errors::default());
            assert_eq!(faults::take_posted(), Posted::default());
            assert_eq!(buffers::checks(), Checks::default());
            assert_eq!(version::kernel(), Kernel::Current);

            // A read that fails with `EIO`, counted and left uncounted.
            faults::set_errors(Errors {
                eio: 1.0,
                ..Errors::default()
            });
            let file = file();
            let ring = IoUring::new(8).unwrap();
            let mut buf = [0_u8; 8];
            push(&ring, read(&file, buf.as_mut_ptr(), 8));
            assert_eq!(reap_n(&ring, 1)[0].result(), -libc::EIO);

            faults::set_faults(Faults {
                eagain: 1.0,
                hang_odds: 1,
            });
            faults::set_errors(Errors {
                eio: 1.0,
                short_read: 1.0,
                enospc: 1.0,
                eintr: 1.0,
            });
            buffers::set_checks(Checks {
                writes: false,
                reads: false,
                overlap: false,
                file_ranges: true,
            });
            version::set_kernel(Kernel::Linux5_4);
        },
        ITERATIONS,
    );
}

/// A buffer at the same address in every execution, for a read that one
/// execution leaves in flight and the next one makes again.
static mut BUF: [u8; 64] = [0; 64];

/// An op that an execution leaves in flight, on a ring it leaks, is gone in
/// the next execution: the buffer checks there do not take a new read into the
/// same buffer for one that overlaps an op still in flight.
#[test]
fn ops_an_earlier_execution_left_in_flight_are_gone() {
    check_random(
        || {
            let file = file();
            let ring = IoUring::new(8).unwrap();
            push(&ring, read(&file, (&raw mut BUF).cast(), 64));
            ring.submit().unwrap();
            // The execution ends with the read in flight, and its kernel task
            // never runs again.
            std::mem::forget(ring);
        },
        ITERATIONS,
    );
}

/// The ops are numbered from the start in every execution, so a read's buffer
/// holds the same poison in each: a consumer that reads a buffer too early
/// sees the same bytes when its failing schedule replays on its own.
#[test]
fn a_reads_poison_is_the_same_in_every_execution() {
    let poisons = Arc::new(Mutex::new(HashSet::new()));
    let seen = Arc::clone(&poisons);
    check_random(
        move || {
            let file = file();
            let ring = IoUring::new(8).unwrap();
            let mut buf = [0_u8; 16];
            push(&ring, read(&file, buf.as_mut_ptr(), 16));
            ring.submit().unwrap();
            // The read is in flight, never having completed inline.
            seen.lock().unwrap().insert(buf);
            reap_n(&ring, 1);
        },
        ITERATIONS,
    );
    let poisons = poisons.lock().unwrap();
    assert_eq!(poisons.len(), 1, "{poisons:?}");
}

shuttle::lazy_static! {
    static ref RING: IoUring = IoUring::new(8).unwrap();
}

/// A ring in a `lazy_static!` is created anew in every execution, with a
/// kernel task of that execution, and dropped at its end after the execution's
/// storage of the simulation is: one left from an earlier execution would have
/// no kernel task, and its ops would never complete.
#[test]
fn a_ring_in_a_lazy_static_is_a_new_ring_in_every_execution() {
    let fds = Arc::new(Mutex::new(HashSet::new()));
    let seen = Arc::clone(&fds);
    check_random(
        move || {
            push(&RING, opcode::Nop::new().build().user_data(1));
            assert_eq!(reap_n(&RING, 1)[0].user_data(), 1);
            seen.lock().unwrap().insert(RING.as_raw_fd());
        },
        ITERATIONS,
    );
    assert_eq!(
        fds.lock().unwrap().len(),
        1,
        "the ring's fd is the same in every execution"
    );
}

/// Ring fds start over in every execution, so a ring's fd, and the name of its
/// kernel task, are the same in each.
#[test]
fn ring_fds_start_over_in_every_execution() {
    let fds = Arc::new(Mutex::new(HashSet::new()));
    let seen = Arc::clone(&fds);
    check_random(
        move || {
            let first = IoUring::new(8).unwrap();
            let second = IoUring::new(8).unwrap();
            assert_eq!(second.as_raw_fd(), first.as_raw_fd() + 1);
            seen.lock().unwrap().insert(first.as_raw_fd());
        },
        ITERATIONS,
    );
    assert_eq!(fds.lock().unwrap().len(), 1);
}

fn eventfd() -> OwnedFd {
    // SAFETY: plain syscall; the fd it returns is owned by nothing else.
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    assert!(fd >= 0, "eventfd: {}", std::io::Error::last_os_error());
    // SAFETY: `fd` was just created.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

/// An `AsyncFd` waits only on what this execution's rings signal: an eventfd
/// that an earlier execution registered, and that a later one opens with the
/// same number, is not one of them.
#[test]
fn an_async_fd_never_sees_an_earlier_executions_eventfd() {
    check_random(
        || {
            let efd = eventfd();
            let fd: RawFd = efd.as_raw_fd();
            let err = AsyncFd::new(fd).map(drop).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
            let ring = IoUring::new(8).unwrap();
            ring.submitter().register_eventfd(fd).unwrap();
            AsyncFd::new(fd).unwrap();
            // Left registered: the ring goes with the execution.
            std::mem::forget(ring);
        },
        ITERATIONS,
    );
}

/// The settings belong to an execution, so, like the rest of Shuttle, setting
/// them outside a Shuttle test panics.
#[test]
#[should_panic(expected = "outside of a Shuttle test")]
fn setting_faults_outside_a_shuttle_test_panics() {
    faults::set_faults(Faults::default());
}

/// So does building a ring.
#[test]
#[should_panic(expected = "outside of a Shuttle test")]
fn building_a_ring_outside_a_shuttle_test_panics() {
    let _ = IoUring::new(8);
}
