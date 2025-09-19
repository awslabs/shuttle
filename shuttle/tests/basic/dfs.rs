use shuttle::scheduler::DfsScheduler;
use shuttle::sync::Mutex;
use shuttle::{thread, Config, MaxSteps, Runner};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use test_log::test;

// TODO all these tests would be a lot simpler if we had some way for the scheduler to return us
// TODO some statistics about coverage

fn max_steps(n: usize) -> Config {
    let mut config = Config::new();
    config.max_steps = MaxSteps::ContinueAfter(n);
    config
}

#[test]
fn trivial_one_thread() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DfsScheduler::new(None, false);
        let runner = Runner::new(scheduler, Default::default());
        runner.run(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
    }

    assert_eq!(iterations.load(Ordering::SeqCst), 1);
}

#[test]
fn trivial_two_threads() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DfsScheduler::new(None, false);
        let runner = Runner::new(scheduler, Default::default());
        runner.run(move || {
            counter.fetch_add(1, Ordering::SeqCst);

            thread::spawn(|| {});
        });
    }

    assert_eq!(iterations.load(Ordering::SeqCst), 3);
}

/// We have two threads T0 and T1 with the following lifecycle (with letter denoting each step):
/// * T0: initializes (i), spawns T1 (s), acquires lock (a), releases lock (r), finishes (e)
/// * T1:                initializes (I), acquires lock (A), releases lock (R), finishes (E)
///
/// Additionally, T0 and T1 may block before acquiring the lock, which show up as duplicate acquires.
///
/// We have a total of 15 interleavings:
///
/// isareIARE
/// isarIeARE
/// isarIAeRE
/// isarIAREe
/// isaIreARE
/// isaIrAeRE
/// isaIrAREe
/// isIareARE
/// isIarAeRE
/// isIarAREe
/// isIaAIreARE
/// isIaAIrAeRE
/// isIaAIrAREe
/// isIAaREare
/// isIAREare
/// ```
fn two_threads_work(counter: &Arc<AtomicUsize>) {
    counter.fetch_add(1, Ordering::SeqCst);

    let lock = Arc::new(Mutex::new(0));

    {
        let lock = Arc::clone(&lock);
        thread::spawn(move || {
            let mut l = lock.lock().unwrap();
            *l += 1;
        });
    }

    let mut l = lock.lock().unwrap();
    *l += 1;
}

#[test]
fn two_threads() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DfsScheduler::new(None, false);
        let runner = Runner::new(scheduler, Default::default());
        runner.run(move || two_threads_work(&counter));
    }

    // See `two_threads_work` for an illustration of all 16 interleavings.
    assert_eq!(iterations.load(Ordering::SeqCst), 29);
}

#[test]
fn two_threads_depth_4() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DfsScheduler::new(None, false);
        let runner = Runner::new(scheduler, max_steps(4));
        runner.run(move || two_threads_work(&counter));
    }

    // See `two_threads_work` for an illustration of all 4 interleavings up to depth 4.
    assert_eq!(iterations.load(Ordering::SeqCst), 4);
}

#[test]
fn two_threads_depth_5() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DfsScheduler::new(None, false);
        let runner = Runner::new(scheduler, max_steps(5));
        runner.run(move || two_threads_work(&counter));
    }

    // See `two_threads_work` for an illustration of all 7 interleavings up to depth 5.
    assert_eq!(iterations.load(Ordering::SeqCst), 8);
}

#[test]
fn yield_loop_one_thread() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DfsScheduler::new(None, false);
        let runner = Runner::new(scheduler, Default::default());
        runner.run(move || {
            counter.fetch_add(1, Ordering::SeqCst);

            thread::spawn(|| {
                for _ in 0..4 {
                    thread::yield_now();
                }
            });

            // no-op
        });
    }

    // 6 places we can run thread 0: before thread 1 starts, before each of the 4 yields, or last
    assert_eq!(iterations.load(Ordering::SeqCst), 7);
}

#[test]
fn yield_loop_two_threads() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DfsScheduler::new(None, false);
        let runner = Runner::new(scheduler, Default::default());
        runner.run(move || {
            counter.fetch_add(1, Ordering::SeqCst);

            thread::spawn(|| {
                for _ in 0..4 {
                    thread::yield_now();
                }
            });

            for _ in 0..4 {
                thread::yield_now();
            }
        });
    }

    // 2 threads, 5 operations each (thread start + 4 yields)
    // 2*5 choose 5 = 252
    assert_eq!(iterations.load(Ordering::SeqCst), 462);
}

#[test]
fn yield_loop_two_threads_bounded() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DfsScheduler::new(Some(100), false);
        let runner = Runner::new(scheduler, Default::default());
        runner.run(move || {
            counter.fetch_add(1, Ordering::SeqCst);

            thread::spawn(|| {
                for _ in 0..4 {
                    thread::yield_now();
                }
            });

            for _ in 0..4 {
                thread::yield_now();
            }
        });
    }

    assert_eq!(iterations.load(Ordering::SeqCst), 100);
}

#[test]
fn yield_loop_three_threads() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DfsScheduler::new(None, false);
        let runner = Runner::new(scheduler, Default::default());
        runner.run(move || {
            counter.fetch_add(1, Ordering::SeqCst);

            thread::spawn(|| {
                for _ in 0..3 {
                    thread::yield_now();
                }
            });

            thread::spawn(|| {
                for _ in 0..3 {
                    thread::yield_now();
                }
            });

            for _ in 0..3 {
                thread::yield_now();
            }
        });
    }

    assert_eq!(iterations.load(Ordering::SeqCst), 378378);
}

#[test]
fn yield_loop_max_depth() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DfsScheduler::new(None, false);
        let runner = Runner::new(scheduler, max_steps(20));
        runner.run(move || {
            for _ in 0..100 {
                counter.fetch_add(1, Ordering::SeqCst);
                thread::yield_now();
            }
        });
    }

    assert_eq!(iterations.load(Ordering::SeqCst), 20);
}
