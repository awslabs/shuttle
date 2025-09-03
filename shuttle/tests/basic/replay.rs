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

#[test]
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
#[should_panic(expected = "91021000904092940400")]
fn replay_failing() {
    replay(concurrent_increment_buggy, "91021000904092940400")
}

#[test]
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
fn replay_passing() {
    replay(concurrent_increment_buggy, "9102110090205124480000")
}

#[test]
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
fn replay_roundtrip() {
    check_replay_roundtrip(concurrent_increment_buggy, PctScheduler::new(2, 100))
}

#[test]
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
fn replay_roundtrip_file() {
    check_replay_roundtrip_file(concurrent_increment_buggy, PctScheduler::new(2, 100))
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
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
fn replay_deadlock_roundtrip() {
    check_replay_roundtrip(deadlock, PctScheduler::new(2, 100))
}

#[test]
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
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
    let schedule = Schedule::new_from_task_ids(0, vec![0, 0, 1, 2, 0, 0, 1, 2]);
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

#[test]
fn replay_deadlock3_drop_mutex() {
    // Schedule ends with a task holding a Mutex, whose MutexGuard needs to be correctly cleaned up
    let schedule = Schedule::new_from_task_ids(0, vec![0, 0, 1, 1, 2]);
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
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
fn replay_long_schedule() {
    check_replay_roundtrip(long_schedule, RandomScheduler::new(1));
}

#[test]
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
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
        let schedule = Schedule::new_from_task_ids(0, vec![0, 1, 0, 2, 0, 3, 0, 1, 1, 2, 2]);
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
        schedule.push_task(1.into());
        schedule.push_task(0.into());
        schedule.push_task(2.into());
        schedule.push_task(0.into());
        schedule.push_task(3.into());
        schedule.push_random();
        schedule.push_random();
        schedule.push_random();
        schedule.push_task(0.into());
        schedule.push_task(1.into());
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
