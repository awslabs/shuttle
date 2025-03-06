use shuttle::scheduler::{ReplayScheduler, Schedule};
use shuttle::{Runner, thread};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use test_log::test;

fn counter_test() {
    let counter = Arc::new(AtomicI32::new(0));
    let counter1 = Arc::clone(&counter);

    thread::spawn(move || {
        for _ in 0..5 {
            counter1.fetch_add(1, Ordering::SeqCst);
            thread::yield_now();
        }
    });

    thread::spawn(move || {
        for _ in 0..5 {
            let v = counter.fetch_add(-1, Ordering::SeqCst);
            assert_ne!(v, 0);
            thread::yield_now();
        }
    });
}

#[test]
#[should_panic]
fn shrink_before_min() {
    // increment counter to 3 and then decrement to 0 to cause panic
    let schedule = Schedule::new_from_task_ids(0, vec![0, 0, 1, 1, 1, 2, 2, 2, 2]);
    let scheduler = ReplayScheduler::new_from_schedule(schedule);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(counter_test);
}

#[test]
#[should_panic]
fn shrink_after_min() {
    // minimal schedule requires no increments
    let min_schedule = Schedule::new_from_task_ids(0, vec![0, 0, 2]);
    let scheduler = ReplayScheduler::new_from_schedule(min_schedule);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(counter_test);
}
