use regex::Regex;
use shuttle::scheduler::{PCTScheduler, ReplayScheduler};
use shuttle::sync::Mutex;
use shuttle::{thread, Runner};
use std::panic;
use std::sync::Arc;

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
#[should_panic(expected = "34021114aa560000")]
fn replay_failing() {
    let schedule = "34021114aa560000";
    let scheduler = ReplayScheduler::new(schedule);
    let runner = Runner::new(scheduler);
    runner.run(concurrent_increment_buggy);
}

#[test]
fn replay_passing() {
    let schedule = "34021114658a0200";
    let scheduler = ReplayScheduler::new(schedule);
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
        let scheduler = ReplayScheduler::new(schedule);
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
        let scheduler = ReplayScheduler::new(schedule);
        let runner = Runner::new(scheduler);
        runner.run(deadlock);
    })
    .expect_err("replay should panic");
    let output = result.downcast::<String>().unwrap();
    let new_schedule = &re.captures(output.as_str()).expect("output should contain a schedule")[1];

    assert_eq!(new_schedule, schedule);
}
