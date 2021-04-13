use shuttle::scheduler::PctScheduler;
use shuttle::sync::Mutex;
use shuttle::{check_random, thread, Config, MaxSteps, Runner};
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
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
            thread::sleep(Duration::from_millis(1));
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
    let scheduler = PctScheduler::new(1, 20);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(figure5);
}

#[test]
fn one_step() {
    let scheduler = PctScheduler::new(2, 100);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(|| {
        thread::spawn(|| {});
    })
}

// Check that PCT correctly deprioritizes a yielding thread. If it wasn't, there would be some
// iteration of this test where the yielding thread has the highest priority and so the others
// never make progress.
fn yield_spin_loop(use_yield: bool) {
    const NUM_THREADS: usize = 4;

    let scheduler = PctScheduler::new(1, 100);
    let mut config = Config::new();
    config.max_steps = MaxSteps::FailAfter(50);
    let runner = Runner::new(scheduler, config);
    runner.run(move || {
        let count = Arc::new(AtomicUsize::new(0usize));

        let _thds = (0..NUM_THREADS)
            .map(|_| {
                let count = count.clone();
                thread::spawn(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect::<Vec<_>>();

        while count.load(Ordering::SeqCst) < NUM_THREADS {
            if use_yield {
                thread::yield_now();
            } else {
                thread::sleep(Duration::from_millis(1));
            }
        }
    })
}

#[test]
fn yield_spin_loop_fair() {
    yield_spin_loop(true);
}

#[test]
#[should_panic(expected = "exceeded max_steps bound")]
fn yield_spin_loop_unfair() {
    yield_spin_loop(false);
}
