//! Every choice the simulation makes is a draw from Shuttle's random number
//! generator, which records it in the schedule: the same seed gives the same
//! completions, and running a schedule again repeats them.

#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Mutex};

use shuttle::{check_random_with_seed, check_uncontrolled_nondeterminism, future};
use shuttle_io_uring_impl::faults::{self, Errors, Faults};
use shuttle_io_uring_impl::types::{Fd, SubmitArgs, Timespec};
use shuttle_io_uring_impl::{AsyncFd, IoUring, opcode, squeue};

const BLOCK: usize = 4096;

fn push(ring: &IoUring, entry: squeue::Entry) {
    // SAFETY: the workload keeps its buffers alive until their completions are
    // reaped, and pushes from one task.
    unsafe { ring.submission_shared().push(&entry) }.expect("SQ has room");
}

fn eventfd() -> OwnedFd {
    // SAFETY: plain syscall; the fd it returns is owned by nothing else.
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    assert!(fd >= 0, "eventfd: {}", std::io::Error::last_os_error());
    // SAFETY: `fd` was just created.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

/// A workload that makes every kind of choice the simulation draws: inline
/// completion, completion order, refusals, short reads and `ENOSPC`, a cancel,
/// timers and timed waits, and interrupted waits, from two tasks, one of them
/// waiting on the ring's eventfd. It returns what the ring posted, in the order
/// it was reaped.
fn workload() -> Vec<(u64, i32)> {
    faults::set_faults(Faults {
        eagain: 0.3,
        ..Faults::default()
    });
    faults::set_errors(Errors {
        short_read: 0.3,
        enospc: 0.3,
        eintr: 0.2,
        ..Errors::default()
    });
    let file = tempfile::tempfile().expect("temp file");
    file.write_all_at(&[7_u8; 3 * BLOCK], 0).unwrap();
    let fd = Fd(file.as_raw_fd());
    let ring = IoUring::new(16).unwrap();
    let efd = eventfd();
    ring.submitter().register_eventfd(efd.as_raw_fd()).unwrap();
    let async_fd = AsyncFd::new(efd).unwrap();
    let woken = future::spawn(async move {
        let mut guard = async_fd.readable().await.unwrap();
        guard.clear_ready();
    });

    let mut bufs = vec![vec![0_u8; 2 * BLOCK]; 3];
    for (i, buf) in bufs.iter_mut().enumerate() {
        push(
            &ring,
            opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
                .offset((i * BLOCK) as u64)
                .build()
                .user_data(i as u64),
        );
    }
    let ts = Timespec::new().sec(1);
    push(&ring, opcode::Timeout::new(&raw const ts).build().user_data(10));
    push(
        &ring,
        opcode::Nop::new().build().flags(squeue::Flags::ASYNC).user_data(11),
    );
    ring.submit().unwrap();
    push(&ring, opcode::AsyncCancel::new(10).build().user_data(12));
    push(
        &ring,
        opcode::Fallocate::new(fd, BLOCK as u64)
            .offset((4 * BLOCK) as u64)
            .build()
            .user_data(13),
    );
    ring.submit().unwrap();

    let mut seen = Vec::new();
    let wait = Timespec::new().nsec(1_000_000);
    let args = SubmitArgs::new().timespec(&wait);
    while seen.len() < 7 {
        let _ = ring.submitter().submit_with_args(1, &args);
        // SAFETY: the only CQ handle.
        seen.extend(unsafe { ring.completion_shared() }.map(|c| (c.user_data(), c.result())));
    }
    future::block_on(woken).unwrap();
    seen
}

/// Runs `workload` in each of `executions` executions of a random scheduler
/// seeded with `seed`, and returns what it returned in each.
fn per_execution(seed: u64, executions: usize) -> Vec<Vec<(u64, i32)>> {
    // A `std::sync::Mutex` on purpose: it holds what the executions produced,
    // and so has to outlive them.
    let collected = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&collected);
    check_random_with_seed(move || sink.lock().unwrap().push(workload()), seed, executions);
    let collected = collected.lock().unwrap().clone();
    assert_eq!(collected.len(), executions);
    collected
}

#[test]
fn the_same_seed_gives_the_same_completions() {
    let first = per_execution(0x5eed, 50);
    assert_eq!(per_execution(0x5eed, 50), first);
    let distinct: HashSet<_> = first.iter().collect();
    assert!(
        distinct.len() > 10,
        "the executions differ: {} distinct of 50",
        distinct.len()
    );
    assert_ne!(per_execution(0x0dd, 50), first, "another seed gives other completions");
}

#[test]
fn the_simulation_is_no_uncontrolled_nondeterminism() {
    check_uncontrolled_nondeterminism(
        || {
            workload();
        },
        100,
    );
}
