use crate::{check_replay_roundtrip, check_replay_roundtrip_file};
use shuttle::scheduler::{PCTScheduler, ReplayScheduler, Schedule};
use shuttle::sync::Mutex;
use shuttle::{replay, thread, Runner};
use std::sync::Arc;
use test_env_log::test;

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

    assert_eq!(*lock.lock().unwrap(), 2, "both threads should run");
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
    check_replay_roundtrip(concurrent_increment_buggy, PCTScheduler::new(2, 100))
}

#[test]
fn replay_roundtrip_file() {
    check_replay_roundtrip_file(concurrent_increment_buggy, PCTScheduler::new(2, 100))
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
    check_replay_roundtrip(deadlock, PCTScheduler::new(2, 100))
}

#[test]
fn replay_deadlock_roundtrip_file() {
    check_replay_roundtrip_file(deadlock, PCTScheduler::new(2, 100))
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
    let schedule = Schedule::new_from_task_ids(0, vec![0, 0, 1, 2, 1, 2, 0, 0]);
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
