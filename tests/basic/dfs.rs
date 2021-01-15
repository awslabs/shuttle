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
        let scheduler = DFSScheduler::new(None, None, false);
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
        let scheduler = DFSScheduler::new(None, None, false);
        let runner = Runner::new(scheduler);
        runner.run(move || {
            counter.fetch_add(1, Ordering::SeqCst);

            thread::spawn(|| {});
        });
    }

    assert_eq!(iterations.load(Ordering::SeqCst), 2);
}

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
        let scheduler = DFSScheduler::new(None, None, false);
        let runner = Runner::new(scheduler);
        runner.run(move || two_threads_work(&counter));
    }

    // 2 threads, 3 operations each (thread start, lock acq, lock rel)
    // 2*3 choose 3 = 20
    assert_eq!(iterations.load(Ordering::SeqCst), 20);
}

#[test]
fn two_threads_depth_4() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DFSScheduler::new(None, Some(4), false);
        let runner = Runner::new(scheduler);
        runner.run(move || two_threads_work(&counter));
    }

    // We have two threads T0 and T1 with the following lifecycle (with letter denoting each step):
    //    T0: spawns T1 (S), waits for lock (W), acquires + releases lock (L), finishes (F)
    //    T1: waits for lock (w), acquires + releases lock (l), finishes (f)
    //
    // We have the following constraints:
    //    operations in each thread are done in order
    //    S happens before w
    //
    // The set of valid interleavings of depth 4 is therefore:
    //    { SWLF, SWLw, SWwL, SWwl, SwWL, SwWl, SwlW, Swlf }
    // for a total of 8 interleavings
    assert_eq!(iterations.load(Ordering::SeqCst), 8);
}

#[test]
fn two_threads_depth_5() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DFSScheduler::new(None, Some(5), false);
        let runner = Runner::new(scheduler);
        runner.run(move || two_threads_work(&counter));
    }

    // We have two threads T0 and T1 with the following lifecycle (with letter denoting each step):
    //    T0: spawns T1 (S), waits for lock (W), acquires + releases lock (L), finishes (F)
    //    T1: waits for lock (w), acquires + releases lock (l), finishes (f)
    //
    // We have the following constraints:
    //    operations in each thread are done in order
    //    S happens before w
    //
    // The set of valid interleavings of depth 5 is therefore:
    //    { SWLFw, SWLwF, SWwLF, SwWLF,                // 4 ops by T0, 1 op  by T1
    //      SWLwl, SWwLl, SWwlL, SwWLl, SwWlL, SwlWL,  // 3 ops by T0, 2 ops by T1
    //      SWwlf, SwWlf, SwlWf, SwlfW }               // 2 ops by T0, 3 ops by T1
    // for a total of 14 interleavings
    assert_eq!(iterations.load(Ordering::SeqCst), 14);
}

#[test]
fn yield_loop_one_thread() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DFSScheduler::new(None, None, false);
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
        let scheduler = DFSScheduler::new(None, None, false);
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
        let scheduler = DFSScheduler::new(Some(100), None, false);
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
        let scheduler = DFSScheduler::new(None, None, false);
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

#[test]
fn yield_loop_max_depth() {
    let iterations = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&iterations);
        let scheduler = DFSScheduler::new(None, Some(20), false);
        let runner = Runner::new(scheduler);
        runner.run(move || {
            for _ in 0..100 {
                counter.fetch_add(1, Ordering::SeqCst);
                thread::yield_now();
            }
        });
    }

    assert_eq!(iterations.load(Ordering::SeqCst), 20);
}
