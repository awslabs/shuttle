//! Where the simulation takes scheduling points, counted exactly.
//!
//! `current::context_switches` counts the scheduling points of the whole
//! execution, and every ring has a kernel task of its own. So these tests run
//! under a scheduler that keeps the test's own task running whenever it can:
//! the kernel task runs only while the test's task is blocked, and the
//! scheduling points of an operation that does not block are the operation's
//! own.

#![cfg(target_os = "linux")]

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use shuttle::scheduler::{DataSource, RandomDataSource, Schedule, Scheduler, Task, TaskId};
use shuttle::{Config, Runner, current};
use shuttle_io_uring_impl::{IoUring, cqueue, opcode, squeue};

fn push(ring: &IoUring, entry: squeue::Entry) {
    // SAFETY: the tests push only nops, which have no buffers, from one task.
    unsafe { ring.submission_shared().push(&entry) }.expect("SQ has room");
}

fn reap(ring: &IoUring) -> Vec<cqueue::Entry> {
    // SAFETY: the tests reap from one task.
    unsafe { ring.completion_shared() }.collect()
}

fn eventfd() -> OwnedFd {
    // SAFETY: plain syscall; the fd it returns is owned by nothing else.
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    assert!(fd >= 0, "eventfd: {}", std::io::Error::last_os_error());
    // SAFETY: `fd` was just created.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

/// Runs the test's task (task 0) whenever it is runnable, and otherwise the
/// first runnable task.
struct TestTaskFirst {
    executions: usize,
    data: RandomDataSource,
}

impl Scheduler for TestTaskFirst {
    fn new_execution(&mut self) -> Option<Schedule> {
        if self.executions == 0 {
            return None;
        }
        self.executions -= 1;
        Some(Schedule::new(self.data.reinitialize()))
    }

    fn next_task(&mut self, runnable: &[&Task], _current: Option<TaskId>, _is_yielding: bool) -> Option<TaskId> {
        let test_task = runnable.iter().find(|task| usize::from(task.id()) == 0);
        Some(test_task.unwrap_or(&runnable[0]).id())
    }

    fn next_u64(&mut self) -> u64 {
        self.data.next_u64()
    }
}

/// Runs `f` in `executions` executions under [`TestTaskFirst`].
fn run(executions: usize, f: impl Fn() + Send + Sync + 'static) {
    let scheduler = TestTaskFirst {
        executions,
        data: RandomDataSource::initialize(0),
    };
    Runner::new(scheduler, Config::new()).run(f);
}

/// How many scheduling points `f` takes.
fn points<T>(f: impl FnOnce() -> T) -> (usize, T) {
    let before = current::context_switches();
    let result = f();
    (current::context_switches() - before, result)
}

/// A ring. Its kernel task runs only while the test's task is blocked.
fn ring() -> IoUring {
    IoUring::new(8).unwrap()
}

/// Building a ring spawns its kernel task, which takes a scheduling point as
/// spawning any task does. Dropping it takes none.
#[test]
fn building_a_ring_takes_one() {
    run(1, || {
        let (n, ring) = points(|| IoUring::new(8).unwrap());
        assert_eq!(n, 1, "building");
        assert_eq!(points(|| drop(ring)).0, 0, "dropping");
    });
}

/// A completion queue handle loads the CQ tail when it is created and when it
/// syncs: one scheduling point each. Taking entries from it takes none.
#[test]
fn a_completion_queue_takes_one_per_tail_load() {
    run(1, || {
        let mut ring = ring();
        let (n, ()) = points(|| {
            ring.completion();
        });
        assert_eq!(n, 1, "completion()");
        let (n, ()) = points(|| {
            let mut cq = ring.completion();
            cq.sync();
            cq.sync();
        });
        assert_eq!(n, 3, "completion() and two syncs");

        // Two completions, posted while the test's task waits for them.
        for ud in 1..=2 {
            push(
                &ring,
                opcode::Nop::new().build().flags(squeue::Flags::ASYNC).user_data(ud),
            );
        }
        ring.submit_and_wait(2).unwrap();
        let mut cq = ring.completion();
        let (n, taken) = points(|| cq.by_ref().count());
        assert_eq!((n, taken), (0, 2), "taking two entries");
    });
}

/// Each access to the CQ flags word is one scheduling point.
#[test]
fn the_eventfd_flag_takes_one_per_access() {
    run(1, || {
        let mut ring = ring();
        let cq = ring.completion();
        assert_eq!(points(|| cq.eventfd_disabled()), (1, false));
        assert_eq!(points(|| cq.disable_eventfd()).0, 1);
        assert_eq!(points(|| cq.eventfd_disabled()), (1, true));
        assert_eq!(points(|| cq.enable_eventfd()).0, 1);
    });
}

/// Without `SQPOLL`, the kernel reads the submission queue only in an enter:
/// pushing and syncing are not scheduling points, and neither is an enter that
/// leaves its op in flight.
#[test]
fn submitting_takes_none() {
    run(1, || {
        let mut ring = ring();
        let (n, ()) = points(|| {
            let mut sq = ring.submission();
            // SAFETY: a nop has no buffers.
            unsafe { sq.push(&opcode::Nop::new().build().flags(squeue::Flags::ASYNC).user_data(1)) }.unwrap();
            sq.sync();
            // SAFETY: as above.
            unsafe { sq.push(&opcode::Nop::new().build().flags(squeue::Flags::ASYNC).user_data(2)) }.unwrap();
        });
        assert_eq!(n, 0, "pushing and syncing");
        assert_eq!(
            points(|| ring.submit().unwrap()),
            (0, 2),
            "an enter that submits two ops"
        );
        ring.submit_and_wait(2).unwrap();
    });
}

/// An op that completes inline posts during the enter: the store of the CQ tail
/// is the enter's one scheduling point, and with an eventfd registered the load
/// of the flags word another. An op that goes in flight takes none.
#[test]
fn an_inline_completion_takes_the_posts_scheduling_points() {
    run(64, || {
        let ring = ring();
        let efd = eventfd();
        for eventfd_points in [0, 1] {
            if eventfd_points == 1 {
                ring.submitter().register_eventfd(efd.as_raw_fd()).unwrap();
            }
            push(&ring, opcode::Nop::new().build().user_data(1));
            let (n, _) = points(|| ring.submit().unwrap());
            // Under this scheduler no other task runs at the tail load, so a
            // completion seen here posted inline.
            // SAFETY: the only CQ handle.
            let inline = unsafe { ring.completion_shared() }.len() == 1;
            if inline {
                assert_eq!(n, 1 + eventfd_points, "an inline completion");
            } else {
                assert_eq!(n, 0, "an op that goes in flight");
                ring.submit_and_wait(1).unwrap();
            }
            reap(&ring);
        }
    });
}

/// The kernel task takes a scheduling point before each completion it posts
/// (the CQ tail store), and one after it (a yield), so that every task can run
/// between the kernel's steps; blocking the test's task on the completion is
/// its one scheduling point.
#[test]
fn a_completion_by_the_kernel_task_takes_three() {
    run(1, || {
        let ring = ring();
        push(
            &ring,
            opcode::Nop::new().build().flags(squeue::Flags::ASYNC).user_data(1),
        );
        ring.submit().unwrap();
        // The test's task blocks (1); the kernel task, woken by the submit,
        // completes the op and stores the tail (1), which wakes the test's
        // task, then yields (1) to it.
        let (n, _) = points(|| ring.submit_and_wait(1).unwrap());
        assert_eq!(n, 3);
        assert_eq!(reap(&ring).len(), 1);
    });
}
