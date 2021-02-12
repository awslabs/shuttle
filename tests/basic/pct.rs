use shuttle::scheduler::PCTScheduler;
use shuttle::sync::Mutex;
use shuttle::{check_random, thread, Runner};
use std::sync::Arc;
use test_env_log::test;

const TEST_LENGTH: usize = 20;

/// Based on Fig 5 of the PCT paper. A randomized scheduler struggles here because it must choose
/// to continually schedule thread 1 until it terminates, which happens with chance 2^TEST_LENGTH.
/// On the other hand, this is a bug depth of 1, so PCT should find it with p = 1/2.
fn figure5() {
    let lock = Arc::new(Mutex::new(0usize));
    let lock_clone = Arc::clone(&lock);

    thread::spawn(move || {
        for _ in 0..TEST_LENGTH {
            thread::yield_now();
        }

        *lock_clone.lock().unwrap() = 1;
    });

    let l = lock.lock().unwrap();
    assert_ne!(*l, 1, "thread 1 ran to completion");
}

#[test]
fn figure5_random() {
    // Chance of missing the bug is (1 - 2^-20)^100 ~= 99.99%, so this should not trip the assert
    check_random(figure5, 100);
}

#[test]
#[should_panic(expected = "thread 1 ran to completion")]
fn figure5_pct() {
    // Change of hitting the bug should be 1 - (1 - 1/2)^20 > 99.9999%, so this should trip the assert
    let scheduler = PCTScheduler::new(1, 20);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(figure5);
}

#[test]
fn one_step() {
    let scheduler = PCTScheduler::new(2, 100);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(|| {
        thread::spawn(|| {});
    })
}
