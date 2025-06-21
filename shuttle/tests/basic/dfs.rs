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

    assert_eq!(iterations.load(Ordering::SeqCst), 2);
}

/// We have two threads T0 and T1 with the following lifecycle (with letter denoting each step):
/// * T0: spawns T1 (S), acquires lock (A), releases lock (R), finishes (F)
/// * T1:                acquires lock (a), releases lock (r), finishes (f)
///
/// Additionally, T0 and T1 may block before acquiring the lock, which we denote by B and b,
/// respectively.
///
/// We have a total of 16 interleavings: 8 where the critical sections do not overlap (4 where
/// T0 goes first and 4 symmetric ones where T1 goes first), and 8 where the critical sections
/// overlap (4 where T0 goes first and T1 blocks, and 4 symmetric ones where T1 goes first and
/// T0 blocks). The following computation tree illustrates all interleavings.
///
/// ```
/// digraph G {
///   node[shape=point];
///   "" -> "S" [label="S"];
///   "S" -> "SA" [label="A"];
///   "SA" -> "SAR" [label="R"];
///   "SAR" -> "SARF" [label="F"];
///   "SARF" -> "SARFa" [label="a"];
///   "SARFa" -> "SARFar" [label="r"];
///   "SARFar" -> "SARFarf" [label="f"];
///   "SAR" -> "SARa" [label="a"];
///   "SARa" -> "SARaF" [label="F"];
///   "SARaF" -> "SARaFr" [label="r"];
///   "SARaFr" -> "SARaFrf" [label="f"];
///   "SARa" -> "SARar" [label="r"];
///   "SARar" -> "SARarF" [label="F"];
///   "SARarF" -> "SARarFf" [label="f"];
///   "SARar" -> "SARarf" [label="f"];
///   "SARarf" -> "SARarfF" [label="F"];
///   "SA" -> "SAb" [label="b"];
///   "SAb" -> "SAbR" [label="R"];
///   "SAbR" -> "SAbRF" [label="F"];
///   "SAbRF" -> "SAbRFa" [label="a"];
///   "SAbRFa" -> "SAbRFar" [label="r"];
///   "SAbRFar" -> "SAbRFarf" [label="f"];
///   "SAbR" -> "SAbRa" [label="a"];
///   "SAbRa" -> "SAbRaF" [label="F"];
///   "SAbRaF" -> "SAbRaFr" [label="r"];
///   "SAbRaFr" -> "SAbRaFrf" [label="f"];
///   "SAbRa" -> "SAbRar" [label="r"];
///   "SAbRar" -> "SAbRarF" [label="F"];
///   "SAbRarF" -> "SAbRarFf" [label="f"];
///   "SAbRar" -> "SAbRarf" [label="f"];
///   "SAbRarf" -> "SAbRarfF" [label="F"];
///   "S" -> "Sa" [label="a"];
///   "Sa" -> "SaB" [label="B"];
///   "SaB" -> "SaBr" [label="r"];
///   "SaBr" -> "SaBrA" [label="A"];
///   "SaBrA" -> "SaBrAR" [label="R"];
///   "SaBrAR" -> "SaBrARF" [label="F"];
///   "SaBrARF" -> "SaBrARFf" [label="f"];
///   "SaBrAR" -> "SaBrARf" [label="f"];
///   "SaBrARf" -> "SaBrARfF" [label="F"];
///   "SaBrA" -> "SaBrAf" [label="f"];
///   "SaBrAf" -> "SaBrAfR" [label="R"];
///   "SaBrAfR" -> "SaBrAfRF" [label="F"];
///   "SaBr" -> "SaBrf" [label="f"];
///   "SaBrf" -> "SaBrfA" [label="A"];
///   "SaBrfA" -> "SaBrfAR" [label="R"];
///   "SaBrfAR" -> "SaBrfARF" [label="F"];
///   "Sa" -> "Sar" [label="r"];
///   "Sar" -> "SarA" [label="A"];
///   "SarA" -> "SarAR" [label="R"];
///   "SarAR" -> "SarARF" [label="F"];
///   "SarARF" -> "SarARFf" [label="f"];
///   "SarAR" -> "SarARf" [label="f"];
///   "SarARf" -> "SarARfF" [label="F"];
///   "SarA" -> "SarAf" [label="f"];
///   "SarAf" -> "SarAfR" [label="R"];
///   "SarAfR" -> "SarAfRF" [label="F"];
///   "Sar" -> "Sarf" [label="f"];
///   "Sarf" -> "SarfA" [label="A"];
///   "SarfA" -> "SarfAR" [label="R"];
///   "SarfAR" -> "SarfARF" [label="F"];
/// }
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
    assert_eq!(iterations.load(Ordering::SeqCst), 16);
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

    // See `two_threads_work` for an illustration of all 6 interleavings up to depth 4.
    assert_eq!(iterations.load(Ordering::SeqCst), 6);
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

    // See `two_threads_work` for an illustration of all 10 interleavings up to depth 5.
    assert_eq!(iterations.load(Ordering::SeqCst), 10);
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
    assert_eq!(iterations.load(Ordering::SeqCst), 6);
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
    assert_eq!(iterations.load(Ordering::SeqCst), 252);
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

    assert_eq!(iterations.load(Ordering::SeqCst), 50050);
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
