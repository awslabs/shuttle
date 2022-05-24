use crate::{check_replay_roundtrip, check_replay_roundtrip_file, Config, FailurePersistence};
use shuttle::scheduler::{PctScheduler, ReplayScheduler, Schedule};
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
#[should_panic(expected = "91021000904092940400")]
fn replay_failing() {
    replay(concurrent_increment_buggy, "91021000904092940400")
}

#[test]
fn replay_passing() {
    replay(concurrent_increment_buggy, "9102110090205124480000")
}

#[test]
fn replay_roundtrip() {
    check_replay_roundtrip(concurrent_increment_buggy, PctScheduler::new(2, 100))
}

#[test]
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
