use shuttle::scheduler::DfsScheduler;
use shuttle::sync::Once;
use shuttle::{check_dfs, check_pct, thread, Runner};
use std::collections::HashSet;
use std::panic;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use test_log::test;

fn basic<F>(num_threads: usize, checker: F)
where
    F: FnOnce(Box<dyn Fn() + Send + Sync + 'static>),
{
    let initializer = Arc::new(std::sync::Mutex::new(HashSet::new()));
    let initializer_clone = Arc::clone(&initializer);

    checker(Box::new(move || {
        let once = Arc::new(Once::new());
        let counter = Arc::new(AtomicUsize::new(0));

        assert!(!once.is_completed());

        let threads = (0..num_threads)
            .map(|_| {
                let once = Arc::clone(&once);
                let counter = Arc::clone(&counter);
                let initializer = Arc::clone(&initializer);
                thread::spawn(move || {
                    once.call_once(|| {
                        counter.fetch_add(1, Ordering::SeqCst);
                        initializer.lock().unwrap().insert(thread::current().id());
                    });

                    assert!(once.is_completed());
                    assert_eq!(counter.load(Ordering::SeqCst), 1);
                })
            })
            .collect::<Vec<_>>();

        for thread in threads {
            thread.join().unwrap();
        }

        assert!(once.is_completed());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }));

    assert_eq!(initializer_clone.lock().unwrap().len(), num_threads);
}

#[test]
fn basic_dfs() {
    basic(2, |f| check_dfs(f, None));
}

#[test]
fn basic_pct() {
    basic(10, |f| check_pct(f, 1000, 3));
}

// Same as `basic`, but with a static Once. Static synchronization primitives should be reset across
// executions, so this test should work exactly the same way.
fn basic_static<F>(num_threads: usize, checker: F)
where
    F: FnOnce(Box<dyn Fn() + Send + Sync + 'static>),
{
    static O: Once = Once::new();

    let initializer = Arc::new(std::sync::Mutex::new(HashSet::new()));
    let initializer_clone = Arc::clone(&initializer);

    checker(Box::new(move || {
        let counter = Arc::new(AtomicUsize::new(0));

        assert!(!O.is_completed());

        let threads = (0..num_threads)
            .map(|_| {
                let counter = Arc::clone(&counter);
                let initializer = Arc::clone(&initializer);
                thread::spawn(move || {
                    O.call_once(|| {
                        counter.fetch_add(1, Ordering::SeqCst);
                        initializer.lock().unwrap().insert(thread::current().id());
                    });

                    assert!(O.is_completed());
                    assert_eq!(counter.load(Ordering::SeqCst), 1);
                })
            })
            .collect::<Vec<_>>();

        for thread in threads {
            thread.join().unwrap();
        }

        assert!(O.is_completed());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }));

    assert_eq!(initializer_clone.lock().unwrap().len(), num_threads);
}

#[test]
fn basic_static_dfs() {
    basic_static(2, |f| check_dfs(f, None));
}

#[test]
fn basic_static_pct() {
    basic_static(10, |f| check_pct(f, 1000, 3));
}

// Test that multiple Once cells race for initialization independently
#[test]
fn multiple() {
    static O1: Once = Once::new();
    static O2: Once = Once::new();

    let initializer = Arc::new(std::sync::Mutex::new(HashSet::new()));
    let initializer_clone = Arc::clone(&initializer);

    check_dfs(
        move || {
            let counter = Arc::new(AtomicUsize::new(0));

            let thd = {
                let counter = Arc::clone(&counter);
                thread::spawn(move || {
                    O1.call_once(|| {
                        counter.fetch_add(1, Ordering::SeqCst);
                    });
                    O2.call_once(|| {
                        counter.fetch_add(4, Ordering::SeqCst);
                    });
                })
            };

            O1.call_once(|| {
                counter.fetch_add(2, Ordering::SeqCst);
            });
            O2.call_once(|| {
                counter.fetch_add(8, Ordering::SeqCst);
            });

            thd.join().unwrap();

            let counter = counter.load(Ordering::SeqCst);
            // lower two bits for C1, upper two bits for C2
            assert!(
                counter & (1 + 2) == 1 || counter & (1 + 2) == 2,
                "exactly one of the O1 calls should have run"
            );
            assert!(
                counter & (4 + 8) == 4 || counter & (4 + 8) == 8,
                "exactly one of the O2 calls should have run"
            );
            initializer.lock().unwrap().insert(counter);
        },
        None,
    );

    let initializer = Arc::try_unwrap(initializer_clone).unwrap().into_inner().unwrap();
    assert_eq!(initializer.len(), 4);
    assert!(initializer.contains(&5));
    assert!(initializer.contains(&6));
    assert!(initializer.contains(&9));
    assert!(initializer.contains(&10));
}

// Ensure that concurrent Shuttle tests see an isolated version of a static Once cell. This test is
// best effort, as it spawns OS threads and hopes they race.
#[test]
fn shared_static() {
    static O: Once = Once::new();

    let counter = Arc::new(AtomicUsize::new(0));
    let mut total_executions = 0;

    // Try a bunch of times to provoke the race
    for _ in 0..50 {
        #[allow(clippy::needless_collect)] // https://github.com/rust-lang/rust-clippy/issues/7207
        let threads = (0..3)
            .map(|_| {
                let counter = Arc::clone(&counter);
                std::thread::spawn(move || {
                    let scheduler = DfsScheduler::new(None, false);
                    let runner = Runner::new(scheduler, Default::default());
                    runner.run(move || {
                        let thds = (0..2)
                            .map(|_| {
                                let counter = Arc::clone(&counter);
                                thread::spawn(move || {
                                    O.call_once(|| {
                                        counter.fetch_add(1, Ordering::SeqCst);
                                    });
                                })
                            })
                            .collect::<Vec<_>>();

                        for thd in thds {
                            thd.join().unwrap();
                        }
                    })
                })
            })
            .collect::<Vec<_>>();

        total_executions += threads.into_iter().map(|handle| handle.join().unwrap()).sum::<usize>();
    }

    // The Once cell should be initialized exactly once per test execution, otherwise the tests are
    // incorrectly sharing the Once cell
    assert_eq!(total_executions, counter.load(Ordering::SeqCst));
}

#[test]
fn poison() {
    static O: Once = Once::new();

    check_dfs(
        || {
            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                O.call_once(|| {
                    panic!("expected panic");
                })
            }));
            assert!(result.is_err(), "`call_once` should panic");

            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                O.call_once(|| {
                    // no-op
                });
            }));
            assert!(result.is_err(), "cell should be poisoned");

            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                O.call_once_force(|state| {
                    assert!(state.is_poisoned());
                });
            }));
            assert!(result.is_ok(), "`call_once_force` ignores poison");

            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                O.call_once(|| unreachable!("previous call should have initialized the cell"));
            }));
            assert!(result.is_ok(), "cell should no longer be poisoned");
        },
        None,
    );
}
