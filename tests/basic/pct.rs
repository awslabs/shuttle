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

#[test]
#[should_panic(expected = "null dereference")]
// Based on Fig 1(a) of the PCT paper.  We model NULL pointer dereference with an Option unwrap
fn figure1a_pct() {
    const COUNT: usize = 5usize;
    // n=2, d=1, so probability of finding the bug is at least 1/2
    // So probability of hitting the bug in 20 iterations = 1 - (1 - 1/2)^20 > 99.9%
    let scheduler = PctScheduler::new(1, 20);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(|| {
        let t1 = Arc::new(Mutex::new(None));
        let t2 = Arc::clone(&t1);

        thread::spawn(move || {
            for _ in 0..COUNT {
                thread::sleep(Duration::from_millis(1));
            }
            *t1.lock().unwrap() = Some(1);
            for _ in 0..COUNT {
                thread::sleep(Duration::from_millis(1));
            }
        });

        thread::spawn(move || {
            for _ in 0..COUNT {
                thread::sleep(Duration::from_millis(1));
            }
            let _ = t2.lock().unwrap().expect("null dereference");
            for _ in 0..COUNT {
                thread::sleep(Duration::from_millis(1));
            }
        });
    });
}

// Based on Fig 1(b) from the PCT paper.  We model NULL pointer dereference with an Option unwrap.
fn figure1b(num_threads: usize) {
    assert!(num_threads >= 2);

    let x1 = Arc::new(Mutex::new(Some(1)));
    let x2 = Arc::clone(&x1);

    // Optionally, spawn a bunch of threads that add scheduling choice points, each taking 5 steps
    for _ in 0..num_threads - 2 {
        thread::spawn(|| {
            for _ in 0..5 {
                thread::sleep(Duration::from_millis(1));
            }
        });
    }

    // Main worker threads take 10 steps each
    thread::spawn(move || {
        for _ in 0..5 {
            thread::sleep(Duration::from_millis(1));
        }
        *x1.lock().unwrap() = None;
        for _ in 0..4 {
            thread::sleep(Duration::from_millis(1));
        }
    });

    thread::spawn(move || {
        for _ in 0..4 {
            thread::sleep(Duration::from_millis(1));
        }
        let b = {
            let b = x2.lock().unwrap().is_some();
            b
        };
        if b {
            let _ = x2.lock().unwrap().expect("null dereference");
        }
        for _ in 0..4 {
            thread::sleep(Duration::from_millis(1));
        }
    });
}

#[test]
#[should_panic(expected = "null dereference")]
fn figure1b_pct() {
    // n=2, k=20, d=2, so probability of finding the bug in one iteration is at least 1/(2*20)
    // So probability of hitting the bug in 300 iterations = 1 - (1 - 1/40)^300 > 99.9%
    let scheduler = PctScheduler::new(2, 300);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(|| {
        figure1b(2);
    });
}

#[test]
#[should_panic(expected = "null dereference")]
fn figure1b_pct_with_many_tasks() {
    // Spawn 48 busy threads, each taking 5 steps, plus 2 main threads with 10 steps, so k=260
    // n=50, k=260, d=2, so probability of finding the bug in one iteration is at least 1/(50*260)
    // So probability of hitting the bug in 2000 iterations = 1 - (1 - 1/400)^2000 > 99.9%
    let scheduler = PctScheduler::new(2, 2000);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(|| {
        figure1b(50);
    });
}

#[test]
#[should_panic(expected = "deadlock")]
// Based on Fig 1(c) from the PCT paper.
fn figure_1c() {
    const COUNT: usize = 4usize;
    // n=2, k=2*14, d=2, so probability of finding the bug is at least 1/(2*28)
    // So probability of hitting the bug in 400 iterations = 1 - (1 - 1/56)^400 > 99.9%
    let scheduler = PctScheduler::new(2, 400);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(|| {
        let a1 = Arc::new(Mutex::new(0));
        let a2 = Arc::clone(&a1);
        let b1 = Arc::new(Mutex::new(0));
        let b2 = Arc::clone(&b1);

        thread::spawn(move || {
            for _ in 0..COUNT {
                thread::sleep(Duration::from_millis(1));
            }
            let a = a1.lock().unwrap();
            for _ in 0..COUNT {
                thread::sleep(Duration::from_millis(1));
            }
            let b = b1.lock().unwrap();
            for _ in 0..COUNT {
                thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(*a + *b, 0)
        });

        thread::spawn(move || {
            for _ in 0..COUNT {
                thread::sleep(Duration::from_millis(1));
            }
            let b = b2.lock().unwrap();
            for _ in 0..COUNT {
                thread::sleep(Duration::from_millis(1));
            }
            let a = a2.lock().unwrap();
            for _ in 0..COUNT {
                thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(*a + *b, 0);
        });
    });
}
