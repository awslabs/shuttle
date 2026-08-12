use crate::basic::clocks::me;
use crate::{check_replay_roundtrip, check_replay_roundtrip_file, Config, FailurePersistence};
use shuttle::scheduler::{PctScheduler, RandomScheduler, ReplayScheduler, Schedule};
use shuttle::sync::Mutex;
use shuttle::{replay, thread, Runner};
use std::panic;
use std::sync::Arc;
use test_log::test;

fn concurrent_increment_buggy() {
    let lock = Arc::new(Mutex::new(0usize));

    let threads = (0..2)
        .map(|_| {
            let lock = Arc::clone(&lock);
            thread::spawn(move || {
                let curr = *lock.lock().unwrap();
                *lock.lock().unwrap() = curr + 1;
            })
        })
        .collect::<Vec<_>>();

    for thd in threads {
        thd.join().unwrap();
    }

    // there's a race where both threads read 0 and then set the counter to 1, so this can fail
    assert_eq!(*lock.lock().unwrap(), 2, "counter is wrong");
}

/// A schedule in which both threads read 0 before either writes, so the counter ends at 1.
///
/// Pinned in the legacy fixed-width hex encoding, which Shuttle is required to keep reading, so this
/// doubles as a check that old schedules still replay. Note that it is a schedule Shuttle persisted at
/// the moment of failure, so it stops there and says nothing about the steps taken while the panic
/// unwinds; replaying it must still reproduce the failure without `set_allow_incomplete`.
#[test]
#[should_panic(expected = "counter is wrong")]
fn replay_failing() {
    replay(concurrent_increment_buggy, "910211ed84dcbbe1bd8c946080408922290100")
}

/// A complete schedule in which the two increments do not overlap, so the counter reaches 2 and the
/// test passes.
#[test]
fn replay_passing() {
    replay(concurrent_increment_buggy, "9102120280404922480200")
}

#[test]
fn replay_roundtrip() {
    check_replay_roundtrip(concurrent_increment_buggy, PctScheduler::new(2, 100))
}

#[test]
fn replay_roundtrip_file() {
    check_replay_roundtrip_file(concurrent_increment_buggy, PctScheduler::new(2, 100))
}

/// Run `f` until it fails, persisting the failing schedule to a fresh directory, and return the
/// contents of every schedule file that was written.
fn persist_failing_schedules<F>(f: F, iterations: usize) -> Vec<String>
where
    F: Fn() + Send + Sync + std::panic::RefUnwindSafe + 'static,
{
    let dir = tempfile::tempdir().expect("could not create tempdir");

    let mut config = Config::new();
    config.failure_persistence = FailurePersistence::File(Some(dir.path().to_path_buf()));

    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let runner = Runner::new(RandomScheduler::new(iterations), config);
        runner.run(f);
    }));
    assert!(result.is_err(), "test was supposed to fail");

    let mut schedules = std::fs::read_dir(dir.path())
        .expect("could not read tempdir")
        .map(|entry| {
            let path = entry.expect("bad dir entry").path();
            std::fs::read_to_string(&path).expect("could not read schedule file")
        })
        .collect::<Vec<_>>();
    schedules.sort();
    schedules
}

/// A schedule persisted from a real failure must replay that failure without any special handling.
///
/// This is the end-to-end property that matters: the schedule Shuttle hands you is enough to
/// reproduce the failure. In particular the replay must not need `set_allow_incomplete`, even though
/// the recorded schedule ends at the failure while the replayed execution goes on to make more
/// scheduling decisions as the panic unwinds.
#[test]
fn persisted_schedule_replays_without_allow_incomplete() {
    let schedules = persist_failing_schedules(concurrent_increment_buggy, 100);
    assert_eq!(
        schedules.len(),
        1,
        "expected exactly one schedule file per failure, got {}",
        schedules.len()
    );

    let result = panic::catch_unwind(|| {
        let scheduler = ReplayScheduler::new_from_encoded(&schedules[0]);
        let mut config = Config::new();
        // The replayed failure would otherwise persist a schedule of its own.
        config.failure_persistence = FailurePersistence::None;
        Runner::new(scheduler, config).run(concurrent_increment_buggy);
    });

    let payload = result.expect_err("replay should reproduce the failure");
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("<non-string panic>");
    assert!(
        message.contains("counter is wrong"),
        "replay panicked with {message:?} instead of reproducing the original failure"
    );
}

/// A failure in a later execution must still be reported, even if an earlier execution of the same
/// runner already reported one.
#[test]
fn every_failing_execution_reports_its_own_schedule() {
    // `concurrent_increment_buggy` fails on some but not all schedules, so the runner reaches the
    // failing execution only after some successful ones. If the report were suppressed by state left
    // over from a previous execution, this would come back empty.
    for _ in 0..5 {
        let schedules = persist_failing_schedules(concurrent_increment_buggy, 100);
        assert_eq!(schedules.len(), 1, "expected exactly one schedule per failure");
        assert!(!schedules[0].trim().is_empty(), "persisted an empty schedule");
    }
}

/// A panic *after* the test has finished running is not a Shuttle failure and must not be reported as
/// one.
///
/// The panic hook is installed once per process and stays installed forever, so it has to know when it
/// is inside an execution and when it is not. Otherwise an unrelated panic later in the test thread,
/// including the test harness reporting a failure, gets a "failing schedule" report attached to it,
/// naming a schedule that has nothing to do with what actually went wrong.
#[test]
fn panic_after_test_reports_no_schedule() {
    let dir = tempfile::tempdir().expect("could not create tempdir");

    let mut config = Config::new();
    config.failure_persistence = FailurePersistence::File(Some(dir.path().to_path_buf()));
    Runner::new(RandomScheduler::new(10), config).run(|| {
        let lock = Arc::new(Mutex::new(0usize));
        let thd = {
            let lock = Arc::clone(&lock);
            thread::spawn(move || *lock.lock().unwrap() += 1)
        };
        thd.join().unwrap();
        assert_eq!(*lock.lock().unwrap(), 1);
    });

    let result = panic::catch_unwind(|| panic!("nothing to do with Shuttle"));
    assert!(result.is_err(), "the panic should have been caught");

    let persisted = std::fs::read_dir(dir.path())
        .expect("could not read tempdir")
        .map(|entry| entry.expect("bad dir entry").path())
        .collect::<Vec<_>>();
    assert!(
        persisted.is_empty(),
        "reported a schedule for a panic outside any execution: {persisted:?}"
    );
}

fn deadlock() {
    let lock1 = Arc::new(Mutex::new(0usize));
    let lock2 = Arc::new(Mutex::new(0usize));
    let lock1_clone = Arc::clone(&lock1);
    let lock2_clone = Arc::clone(&lock2);

    thread::spawn(move || {
        let _l1 = lock1_clone.lock().unwrap();
        let _l2 = lock2_clone.lock().unwrap();
    });

    let _l2 = lock2.lock().unwrap();
    let _l1 = lock1.lock().unwrap();
}

#[test]
fn replay_deadlock_roundtrip() {
    check_replay_roundtrip(deadlock, PctScheduler::new(2, 100))
}

#[test]
fn replay_deadlock_roundtrip_file() {
    check_replay_roundtrip_file(deadlock, PctScheduler::new(2, 100))
}

fn deadlock_3() {
    let lock1 = Arc::new(Mutex::new(0usize));
    let lock2 = Arc::new(Mutex::new(0usize));
    let lock3 = Arc::new(Mutex::new(0usize));

    let lock1_clone = Arc::clone(&lock1);
    let lock2_clone = Arc::clone(&lock2);
    let lock3_clone = Arc::clone(&lock3);

    thread::spawn(move || {
        let _l1 = lock1_clone.lock().unwrap();
        let _l2 = lock2_clone.lock().unwrap();
    });

    thread::spawn(move || {
        let _l2 = lock2.lock().unwrap();
        let _l3 = lock3_clone.lock().unwrap();
    });

    let _l3 = lock3.lock().unwrap();
    let _l1 = lock1.lock().unwrap();
}

#[test]
#[should_panic(expected = "deadlock")]
fn replay_deadlock3_block() {
    // Reproduce deadlock
    let schedule = Schedule::new_from_task_ids(0, vec![0, 0, 1, 0, 1, 0, 2, 2, 1]);
    let scheduler = ReplayScheduler::new_from_schedule(schedule);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(deadlock_3);
}

#[test]
fn replay_deadlock3_end_early() {
    // Schedule ends without all tasks finishing
    let schedule = Schedule::new_from_task_ids(0, vec![0, 0, 1, 2]);
    let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
    scheduler.set_allow_incomplete();
    let runner = Runner::new(scheduler, Default::default());
    runner.run(deadlock_3);
}

#[test]
fn replay_deadlock3_task_disabled() {
    // Schedule ends when a task is not runnable
    let schedule = Schedule::new_from_task_ids(0, vec![0, 1, 2, 0]);
    let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
    scheduler.set_allow_incomplete();
    let runner = Runner::new(scheduler, Default::default());
    runner.run(deadlock_3);
}

#[ignore = "this test aborts due to an issue with panic handling on exit with the generators library -- can be removed when we switch to corosensei"]
#[test]
fn replay_deadlock3_drop_mutex() {
    // Schedule ends with a task holding a Mutex, whose MutexGuard needs to be correctly cleaned up
    let schedule = Schedule::new_from_task_ids(0, vec![0, 0, 1, 0, 1, 0]);
    let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
    scheduler.set_allow_incomplete();
    let runner = Runner::new(scheduler, Default::default());
    runner.run(deadlock_3);
}

fn long_schedule() {
    let mut threads = vec![];
    for _ in 0..100 {
        threads.push(shuttle::thread::spawn(|| {
            for _ in 0..100 {
                shuttle::thread::yield_now();
            }
        }));
    }
    for t in threads {
        t.join().unwrap();
    }
    // If this would be a `panic!`, downcasting the `catch_unwind` error to `String` fails.
    assert_eq!(1, 2, "so much work, and all for nothing");
}

#[test]
fn replay_long_schedule() {
    check_replay_roundtrip(long_schedule, RandomScheduler::new(1));
}

#[test]
fn replay_long_schedule_file() {
    check_replay_roundtrip_file(long_schedule, RandomScheduler::new(1));
}

// Check that FailurePersistence::None does not print a schedule
#[test]
fn replay_persist_none() {
    let result = panic::catch_unwind(|| {
        let scheduler = PctScheduler::new(2, 100);
        let mut config = Config::new();
        config.failure_persistence = FailurePersistence::None;
        let runner = Runner::new(scheduler, config);
        runner.run(concurrent_increment_buggy);
    })
    .expect_err("test should panic");
    let output = result.downcast::<String>().unwrap();
    assert!(output.contains("counter is wrong"));
    // All our current failure persistence modes print the word "schedule", so check that's missing
    assert!(!output.contains("schedule"));
}

/// Tests that events not causally related to the failure are never scheduled.
#[test]
fn replay_causality() {
    // The main thread will spawn three threads:
    // - A, which acquires the lock and sets it to one;
    // - B, which acquires the lock and asserts it is zero;
    // - C, which sets an unrelated atomic Boolean.
    // If A runs before B (as in the schedule below), then the assertion
    // fails. If we provide the clock of this failure to the scheduler, we
    // should never see thread C do anything, i.e., the atomic Boolean should
    // never be set, because it is not causally related to the actual panic.

    use std::sync::atomic::{AtomicBool, Ordering};

    let flag = Arc::new(AtomicBool::new(false));
    let flag_clone = Arc::clone(&flag);

    let result = panic::catch_unwind(|| {
        let schedule = Schedule::new_from_task_ids(0, vec![0, 0, 1, 1, 0, 0, 3, 2, 0, 1, 2, 2]);
        let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
        scheduler.set_target_clock(&[2, 2, 1]);
        let mut config = Config::new();
        config.failure_persistence = FailurePersistence::None;
        let runner = Runner::new(scheduler, config);
        runner.run(move || {
            assert_eq!(me(), 0);
            let lock = Arc::new(Mutex::new(0usize));
            let lock_clone = Arc::clone(&lock);
            thread::spawn(move || {
                assert_eq!(me(), 1);
                *lock_clone.lock().unwrap() = 1;
            });
            thread::spawn(move || {
                assert_eq!(me(), 2);
                let guard = lock.lock().unwrap();
                assert!(*guard == 0, "expected panic");
                drop(guard);
            });
            let flag_clone = Arc::clone(&flag_clone);
            thread::spawn(move || {
                assert_eq!(me(), 3);
                // Note that this operation is performed in a separate thread,
                // since the (non-Shuttle) atomic does not increment the clock
                // of the current thread. If the atomic were set instead in the
                // main thread, then the clocks of "setting the atomic" and "B
                // acquiring a lock" would be indistinguishable. However, we
                // need the non-Shuttle atomic to smuggle data out of a this
                // panicking test.
                flag_clone.store(true, Ordering::SeqCst);
            });
        });
    })
    .expect_err("test should panic");
    let output = result.downcast::<&str>().unwrap();
    assert_eq!(*output, "expected panic");

    assert!(!flag.load(Ordering::SeqCst));
}

/// Similar to `replay_causality`, but with a schedule that also contains
/// random choice steps.
#[test]
fn replay_causality_with_random() {
    // The thread setup here is the same as in `replay_causality`, but thread
    // 3 is using the RNG rather than setting a Boolean flag.

    let result = panic::catch_unwind(|| {
        // Manually construct a schedule, to show explicitly the thread steps
        // and the random steps made for thread 3 (which are irrelevant to the
        // failure being replayed).
        let mut schedule = Schedule::new(0);
        schedule.push_task(0.into());
        schedule.push_task(0.into());
        schedule.push_task(1.into());
        schedule.push_task(1.into());
        schedule.push_task(0.into());
        schedule.push_task(0.into());
        schedule.push_task(3.into());
        schedule.push_random();
        schedule.push_random();
        schedule.push_random();
        schedule.push_task(2.into());
        schedule.push_task(0.into());
        schedule.push_task(1.into());
        schedule.push_task(2.into());
        schedule.push_task(2.into());

        let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
        scheduler.set_target_clock(&[2, 2, 1]);
        let mut config = Config::new();
        config.failure_persistence = FailurePersistence::None;
        let runner = Runner::new(scheduler, config);
        runner.run(move || {
            assert_eq!(me(), 0);
            let lock = Arc::new(Mutex::new(0usize));
            let lock_clone = Arc::clone(&lock);
            thread::spawn(move || {
                assert_eq!(me(), 1);
                *lock_clone.lock().unwrap() = 1;
            });
            thread::spawn(move || {
                assert_eq!(me(), 2);
                let guard = lock.lock().unwrap();
                assert!(*guard == 0, "expected panic");
                drop(guard);
            });
            thread::spawn(move || {
                use shuttle::rand::Rng;
                assert_eq!(me(), 3);
                let mut thread_rng = shuttle::rand::thread_rng();
                thread_rng.gen::<u64>();
                thread_rng.gen::<u64>();
                thread_rng.gen::<u64>();
            });
        });
    })
    .expect_err("test should panic");
    let output = result.downcast::<&str>().unwrap();
    assert_eq!(*output, "expected panic");
}
