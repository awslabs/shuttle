use regex::Regex;
use shuttle::scheduler::{PCTScheduler, ReplayScheduler};
use shuttle::sync::Mutex;
use shuttle::{thread, Runner};
use std::panic;
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
#[should_panic(expected = "34021014aa5600")]
fn replay_failing() {
    let schedule = "34021014aa5600";
    let scheduler = ReplayScheduler::new_from_encoded(schedule);
    let runner = Runner::new(scheduler);
    runner.run(concurrent_increment_buggy);
}

#[test]
fn replay_passing() {
    let schedule = "34021114658a0200";
    let scheduler = ReplayScheduler::new_from_encoded(schedule);
    let runner = Runner::new(scheduler);
    runner.run(concurrent_increment_buggy);
}

#[test]
fn replay_roundtrip() {
    let re = Regex::new("schedule: \"([0-9a-f]+)\"").unwrap();

    // Run the test that should fail and capture the schedule it prints
    let result = panic::catch_unwind(|| {
        let scheduler = PCTScheduler::new(2, 100);
        let runner = Runner::new(scheduler);
        runner.run(concurrent_increment_buggy);
    })
    .expect_err("test should panic");
    let output = result.downcast::<String>().unwrap();
    let schedule = &re.captures(output.as_str()).expect("output should contain a schedule")[1];

    // Now replay that schedule and make sure it still fails and outputs the same schedule
    let result = panic::catch_unwind(|| {
        let scheduler = ReplayScheduler::new_from_encoded(schedule);
        let runner = Runner::new(scheduler);
        runner.run(concurrent_increment_buggy);
    })
    .expect_err("replay should panic");
    let output = result.downcast::<String>().unwrap();
    let new_schedule = &re.captures(output.as_str()).expect("output should contain a schedule")[1];

    assert_eq!(new_schedule, schedule);
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
    let re = Regex::new("schedule: \"([0-9a-f]+)\"").unwrap();

    // Run the test that should fail and capture the schedule it prints
    let result = panic::catch_unwind(|| {
        let scheduler = PCTScheduler::new(2, 100);
        let runner = Runner::new(scheduler);
        runner.run(deadlock);
    })
    .expect_err("test should panic");
    let output = result.downcast::<String>().unwrap();
    let schedule = &re.captures(output.as_str()).expect("output should contain a schedule")[1];

    // Now replay that schedule and make sure it still fails and outputs the same schedule
    let result = panic::catch_unwind(|| {
        let scheduler = ReplayScheduler::new_from_encoded(schedule);
        let runner = Runner::new(scheduler);
        runner.run(deadlock);
    })
    .expect_err("replay should panic");
    let output = result.downcast::<String>().unwrap();
    let new_schedule = &re.captures(output.as_str()).expect("output should contain a schedule")[1];

    assert_eq!(new_schedule, schedule);
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
    let schedule = vec![0, 0, 1, 2, 1, 2, 0, 0];
    let scheduler = ReplayScheduler::new_from_schedule(schedule.into());
    let runner = Runner::new(scheduler);
    runner.run(deadlock_3);
}

#[test]
fn replay_deadlock3_end_early() {
    // Schedule ends without all tasks finishing
    let schedule = vec![0, 0, 1, 2];
    let mut scheduler = ReplayScheduler::new_from_schedule(schedule.into());
    scheduler.set_allow_incomplete();
    let runner = Runner::new(scheduler);
    runner.run(deadlock_3);
}

#[test]
fn replay_deadlock3_task_disabled() {
    // Schedule ends when a task is not runnable
    let schedule = vec![0, 1, 2, 0];
    let mut scheduler = ReplayScheduler::new_from_schedule(schedule.into());
    scheduler.set_allow_incomplete();
    let runner = Runner::new(scheduler);
    runner.run(deadlock_3);
}

#[test]
#[ignore]
// TODO This test is currently marked `ignore` because a schedule that terminates
// TODO early while one of the threads is holding a Mutex causes a panic.
// TODO (Because Mutex::drop tries to access the thread-local state outside a set.)
fn replay_deadlock3_drop_mutex() {
    let schedule = vec![0, 0, 1, 1, 2];
    let mut scheduler = ReplayScheduler::new_from_schedule(schedule.into());
    scheduler.set_allow_incomplete();
    let runner = Runner::new(scheduler);
    runner.run(deadlock_3);
}
