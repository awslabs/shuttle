use shuttle::scheduler::{PctScheduler, RandomScheduler};
use shuttle::sync::Mutex;
use shuttle::{thread, PortfolioRunner, Runner};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use test_env_log::test;

#[test]
fn portfolio_success() {
    let f = || {
        const THREADS: usize = 3;
        let counter = Arc::new(Mutex::new(0));
        let threads = (0..THREADS)
            .map(|_| {
                let counter = counter.clone();
                thread::spawn(move || {
                    *counter.lock().unwrap() += 1;
                })
            })
            .collect::<Vec<_>>();
        for thd in threads {
            thd.join().unwrap();
        }
        let counter = *counter.lock().unwrap();
        assert_eq!(counter, THREADS);
    };

    let mut runner = PortfolioRunner::new(true, Default::default());
    runner.add(PctScheduler::new(1, 100));
    runner.add(PctScheduler::new(2, 100));
    runner.add(RandomScheduler::new(100));

    runner.run(f);
}

// A trivial deadlock (locks acquired in opposite orders), but won't be found by PCT at depth 1 because it requires two
// preemptions to hit the deadlock.
fn two_thread_deadlock() {
    let lock1 = Arc::new(Mutex::new(0));
    let lock2 = Arc::new(Mutex::new(0));

    {
        let lock1 = lock1.clone();
        let lock2 = lock2.clone();
        thread::spawn(move || {
            let mut lock1 = lock1.lock().unwrap();
            let mut lock2 = lock2.lock().unwrap();
            *lock1 += 1;
            *lock2 += 1;
        });
    }

    {
        thread::spawn(move || {
            let mut lock2 = lock2.lock().unwrap();
            let mut lock1 = lock1.lock().unwrap();
            *lock1 += 1;
            *lock2 += 1;
        });
    }
}

#[test]
fn two_thread_deadlock_pct_depth_one() {
    // depth 1 shouldn't fail
    let scheduler = PctScheduler::new(1, 1000);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(two_thread_deadlock);
}

#[test]
#[should_panic(expected = "deadlock")]
fn two_thread_deadlock_portfolio() {
    // depth 1 shouldn't fail, but depth 2 should fail very fast
    let mut runner = PortfolioRunner::new(true, Default::default());
    runner.add(PctScheduler::new(1, 100));
    runner.add(PctScheduler::new(2, 100));
    runner.run(two_thread_deadlock);
}

#[test]
fn two_thread_deadlock_portfolio_no_early_stop() {
    // same as two_thread_deadlock_portfolio, but without stopping on first failure, so PCT depth 1
    // should run all 100 iterations
    let mut runner = PortfolioRunner::new(false, Default::default());
    runner.add(PctScheduler::new(1, 100));
    runner.add(PctScheduler::new(2, 100));

    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        runner.run(move || {
            two_thread_deadlock();
            counter.fetch_add(1, Ordering::SeqCst);
        })
    }));
    assert!(result.is_err(), "PCT depth 2 should find the bug");
    assert!(
        counter_clone.load(Ordering::SeqCst) >= 100,
        "PCT depth 1 should have run to completion"
    );
}
