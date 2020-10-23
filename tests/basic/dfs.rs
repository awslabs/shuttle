use shuttle::scheduler::DFSScheduler;
use shuttle::sync::Mutex;
use shuttle::{thread, Runner};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use test_env_log::test;

// TODO all these tests would be a lot simpler if we had some way for the scheduler to return us
// TODO some statistics about coverage

#[test]
fn trivial_one_thread() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DFSScheduler::new(None);
        let runner = Runner::new(scheduler);
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
        let scheduler = DFSScheduler::new(None);
        let runner = Runner::new(scheduler);
        runner.run(move || {
            counter.fetch_add(1, Ordering::SeqCst);

            thread::spawn(|| {});
        });
    }

    assert_eq!(iterations.load(Ordering::SeqCst), 2);
}

#[test]
fn two_threads() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DFSScheduler::new(None);
        let runner = Runner::new(scheduler);
        runner.run(move || {
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
        });
    }

    // 2 threads, 3 operations each (thread start, lock acq, lock rel)
    // 2*3 choose 3 = 20
    assert_eq!(iterations.load(Ordering::SeqCst), 20);
}

#[test]
fn yield_loop_one_thread() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DFSScheduler::new(None);
        let runner = Runner::new(scheduler);
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
    assert_eq!(iterations.load(Ordering::SeqCst), 6);
}

#[test]
fn yield_loop_two_threads() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DFSScheduler::new(None);
        let runner = Runner::new(scheduler);
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
    assert_eq!(iterations.load(Ordering::SeqCst), 252);
}

#[test]
fn yield_loop_two_threads_bounded() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DFSScheduler::new(Some(100));
        let runner = Runner::new(scheduler);
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
        let scheduler = DFSScheduler::new(None);
        let runner = Runner::new(scheduler);
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

    assert_eq!(iterations.load(Ordering::SeqCst), 50050);
}
