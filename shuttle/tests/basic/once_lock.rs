use shuttle::scheduler::{DfsScheduler, RandomScheduler};
use shuttle::sync::OnceLock;
use shuttle::thread::ThreadId;
use shuttle::{check_dfs, check_pct, check_random, check_uncontrolled_nondeterminism, current, thread, Runner};
use std::collections::HashSet;
use std::panic;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use test_log::test;

#[test]
fn basic() {
    check_dfs(
        || {
            let cell = OnceLock::new();
            assert_eq!(cell.get(), None);

            assert_eq!(cell.get_or_init(|| 42), &42);
            assert_eq!(cell.get(), Some(&42));

            // The cell has a value, so neither initializer runs.
            assert_eq!(cell.get_or_init(|| unreachable!("the cell is initialized")), &42);
            assert_eq!(cell.set(43), Err(43));
            assert_eq!(cell.get(), Some(&42));
        },
        None,
    );
}

// A cell takes a scheduling point only where it may block: acquiring and releasing the lock that
// racing initializers contend on. A read of an initialized cell takes none, as `Once::call_once`
// takes none once the `Once` has completed.
#[test]
fn context_switches() {
    check_dfs(
        || {
            let cell = OnceLock::new();
            assert_eq!(current::context_switches(), 1);

            // Reading an uninitialized cell does not block.
            assert_eq!(cell.get(), None);
            assert_eq!(current::context_switches(), 1);

            // Initializing it takes the lock: one scheduling point for the acquire, one for the
            // release.
            assert_eq!(cell.get_or_init(|| 42), &42);
            assert_eq!(current::context_switches(), 3);

            // Reads of an initialized cell do not block, however they are spelled.
            assert_eq!(cell.get(), Some(&42));
            assert_eq!(cell.get_or_init(|| unreachable!("the cell is initialized")), &42);
            assert_eq!(cell.set(43), Err(43));
            assert_eq!(current::context_switches(), 3);

            // Neither do the methods that take the cell by `&mut`.
            let mut cell = cell;
            assert_eq!(cell.get_mut(), Some(&mut 42));
            assert_eq!(cell.take(), Some(42));
            assert_eq!(current::context_switches(), 3);
        },
        None,
    );
}

#[test]
fn set_and_take() {
    check_dfs(
        || {
            let mut cell = OnceLock::new();
            assert_eq!(cell.take(), None);
            assert_eq!(cell.set("hello".to_string()), Ok(()));
            assert_eq!(cell.set("goodbye".to_string()), Err("goodbye".to_string()));

            // Taking the value leaves the cell uninitialized, so it can be initialized again.
            assert_eq!(cell.take(), Some("hello".to_string()));
            assert_eq!(cell.get(), None);
            assert_eq!(cell.set("hello again".to_string()), Ok(()));
            *cell.get_mut().unwrap() = "mutated".to_string();
            assert_eq!(cell.into_inner(), Some("mutated".to_string()));
        },
        None,
    );
}

#[test]
fn try_init() {
    check_dfs(
        || {
            let cell = OnceLock::new();

            // A failed initializer leaves the cell uninitialized.
            assert_eq!(cell.get_or_try_init(|| Err::<usize, _>("no")), Err("no"));
            assert_eq!(cell.get(), None);

            assert_eq!(cell.get_or_try_init(|| Ok::<_, &str>(42)), Ok(&42));
            assert_eq!(cell.get(), Some(&42));
        },
        None,
    );
}

// Run `num_threads` threads racing to initialize a cell and record which of them won. Check that
// we saw every thread win, so that any of them can win the race, and that exactly one initializer
// ran in each execution.
#[track_caller]
fn run_race_test(num_threads: usize, tester: impl FnOnce(Box<dyn Fn() + Send + Sync>)) {
    let winners = Arc::new(Mutex::new(HashSet::new()));
    let winners_clone = Arc::clone(&winners);

    tester(Box::new(move || {
        let cell = Arc::new(OnceLock::new());
        let initializers = Arc::new(AtomicUsize::new(0));

        let threads = (0..num_threads)
            .map(|i| {
                let cell = Arc::clone(&cell);
                let initializers = Arc::clone(&initializers);
                thread::spawn(move || {
                    let value = cell.get_or_init(|| {
                        initializers.fetch_add(1, Ordering::SeqCst);
                        i
                    });
                    // Every thread sees the same value, whichever initializer won.
                    assert_eq!(*cell.get().unwrap(), *value);
                })
            })
            .collect::<Vec<_>>();

        for thread in threads {
            thread.join().unwrap();
        }

        assert_eq!(initializers.load(Ordering::SeqCst), 1, "exactly one initializer runs");
        winners.lock().unwrap().insert(*cell.get().unwrap());
    }));

    assert_eq!(
        winners_clone.lock().unwrap().len(),
        num_threads,
        "every thread should win the race in some execution"
    );
}

#[test]
fn racing_init_dfs() {
    run_race_test(2, |f| check_dfs(f, None));
}

#[test]
fn racing_init_pct() {
    run_race_test(4, |f| check_pct(f, 1000, 3));
}

// A `static` cell is empty again in every execution, and its initializer runs once per execution.
#[test]
fn static_resets_across_executions() {
    static CELL: OnceLock<usize> = OnceLock::new();
    static INITIALIZERS: AtomicUsize = AtomicUsize::new(0);

    INITIALIZERS.store(0, Ordering::SeqCst);

    let runner = Runner::new(DfsScheduler::new(None, false), Default::default());
    let executions = runner.run(|| {
        // Nothing carried over from the execution before this one.
        assert_eq!(CELL.get(), None);

        let threads = (0..2)
            .map(|_| {
                thread::spawn(|| {
                    assert_eq!(
                        *CELL.get_or_init(|| {
                            INITIALIZERS.fetch_add(1, Ordering::SeqCst);
                            42
                        }),
                        42
                    );
                })
            })
            .collect::<Vec<_>>();

        for thread in threads {
            thread.join().unwrap();
        }
    });

    assert!(executions > 1, "DFS should find more than one schedule");
    assert_eq!(
        INITIALIZERS.load(Ordering::SeqCst),
        executions,
        "the initializer should run exactly once per execution"
    );
}

// It is valid for an initializer to block, so other threads have to wait for it.
#[test]
fn init_with_yield() {
    check_dfs(
        || {
            let cell = Arc::new(OnceLock::new());
            let cell_clone = Arc::clone(&cell);

            let thread = thread::spawn(move || {
                assert_eq!(*cell_clone.get_or_init(|| 1), 1);
            });

            assert_eq!(
                *cell.get_or_init(|| {
                    thread::yield_now();
                    1
                }),
                1
            );

            thread.join().unwrap();
        },
        None,
    );
}

// An initializer that panics leaves the cell uninitialized, and a later caller runs its own
// initializer, as `std`'s `OnceLock` does (it ignores the poisoning of its inner `Once`).
#[test]
fn panicking_initializer_leaves_cell_uninitialized() {
    check_dfs(
        || {
            let cell = OnceLock::new();

            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                cell.get_or_init(|| panic!("expected panic"));
            }));
            assert!(result.is_err(), "the panic should be propagated");
            assert_eq!(cell.get(), None, "the cell should be uninitialized");

            // The cell can still be initialized, by an initializer that panics again...
            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                cell.get_or_init(|| panic!("expected panic"));
            }));
            assert!(result.is_err(), "the panic should be propagated");
            assert_eq!(cell.get(), None);

            // ... or by one that succeeds.
            assert_eq!(cell.get_or_init(|| 42), &42);
            assert_eq!(cell.get(), Some(&42));
        },
        None,
    );
}

// A cell whose initializer panicked can be initialized by another thread.
#[test]
fn panicking_initializer_lets_another_thread_initialize() {
    check_dfs(
        || {
            let cell = Arc::new(OnceLock::new());
            let cell_clone = Arc::clone(&cell);

            let thread = thread::spawn(move || {
                // Either this thread wins the race and its initializer panics, or the other
                // thread initialized the cell first and this initializer never runs.
                let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    *cell_clone.get_or_init(|| panic!("expected panic"))
                }));
                if let Ok(value) = result {
                    assert_eq!(value, 42);
                }
            });

            let value = *cell.get_or_init(|| 42);
            assert_eq!(value, 42);

            thread.join().unwrap();
            assert_eq!(cell.get(), Some(&42));
        },
        None,
    );
}

// Initializing a cell from its own initializer deadlocks in `std`; Shuttle reports it.
#[test]
#[should_panic(expected = "deadlock")]
fn reentrant_init() {
    check_dfs(
        || {
            let cell = Arc::new(OnceLock::new());
            let cell_clone = Arc::clone(&cell);
            cell.get_or_init(|| *cell_clone.get_or_init(|| 42));
        },
        None,
    );
}

// Replaying a seed reproduces the same sequence of events, which it cannot do if a cell keeps
// anything from one execution to the next.
#[test]
fn replay_from_seed() {
    static CELL: OnceLock<usize> = OnceLock::new();

    fn run(seed: u64) -> Vec<(ThreadId, usize)> {
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_clone = Arc::clone(&log);

        let runner = Runner::new(RandomScheduler::new_from_seed(seed, 50), Default::default());
        runner.run(move || {
            let threads = (0..3)
                .map(|i| {
                    let log = Arc::clone(&log);
                    thread::spawn(move || {
                        let value = *CELL.get_or_init(|| i);
                        log.lock().unwrap().push((thread::current().id(), value));
                    })
                })
                .collect::<Vec<_>>();

            for thread in threads {
                thread.join().unwrap();
            }
        });

        Arc::try_unwrap(log_clone).unwrap().into_inner().unwrap()
    }

    let first = run(0x5EED);
    let second = run(0x5EED);
    assert!(!first.is_empty());
    assert_eq!(first, second);

    // A different seed explores different schedules, so the two runs should disagree somewhere.
    assert_ne!(first, run(0xD1FF));
}

#[test]
fn uncontrolled_nondeterminism() {
    static CELL: OnceLock<usize> = OnceLock::new();

    check_uncontrolled_nondeterminism(
        || {
            let threads = (0..3)
                .map(|i| thread::spawn(move || *CELL.get_or_init(|| i)))
                .collect::<Vec<_>>();

            let values = threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<HashSet<_>>();
            assert_eq!(values.len(), 1, "every thread sees the same value");
        },
        100,
    );
}

// Ensure that concurrent Shuttle tests see an isolated version of a `static` cell. This test is
// best effort, as it spawns OS threads and hopes they race.
#[test]
fn shared_static() {
    static CELL: OnceLock<usize> = OnceLock::new();
    static INITIALIZERS: AtomicUsize = AtomicUsize::new(0);

    INITIALIZERS.store(0, Ordering::SeqCst);
    let mut total_executions = 0;

    // Try a bunch of times to provoke the race
    for _ in 0..50 {
        let threads = (0..3)
            .map(|_| {
                std::thread::spawn(move || {
                    let runner = Runner::new(DfsScheduler::new(None, false), Default::default());
                    runner.run(|| {
                        let threads = (0..2)
                            .map(|_| {
                                thread::spawn(|| {
                                    assert_eq!(
                                        *CELL.get_or_init(|| {
                                            INITIALIZERS.fetch_add(1, Ordering::SeqCst);
                                            42
                                        }),
                                        42
                                    );
                                })
                            })
                            .collect::<Vec<_>>();

                        for thread in threads {
                            thread.join().unwrap();
                        }
                    })
                })
            })
            .collect::<Vec<_>>();

        total_executions += threads.into_iter().map(|handle| handle.join().unwrap()).sum::<usize>();
    }

    // The cell should be initialized exactly once per execution, otherwise the tests are
    // incorrectly sharing it
    assert_eq!(total_executions, INITIALIZERS.load(Ordering::SeqCst));
}

// `wait` on a cell that has a value returns at once and takes no scheduling point; on an empty
// cell it blocks, and the switch away from the waiting thread is its one scheduling point.
#[test]
fn wait_context_switches() {
    check_dfs(
        || {
            let cell = OnceLock::new();
            cell.set(1).unwrap();
            let before = current::context_switches();
            assert_eq!(cell.wait(), &1);
            assert_eq!(current::context_switches(), before);
        },
        None,
    );

    check_dfs(
        || {
            let cell = Arc::new(OnceLock::new());
            let setter = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || cell.set(1).unwrap())
            };
            // The spawned thread does not run until this one reaches a scheduling point, so the
            // cell is still empty.
            let before = current::context_switches();
            assert_eq!(cell.wait(), &1);
            assert!(current::context_switches() > before);
            setter.join().unwrap();
        },
        None,
    );
}

// A waiter sees the value whichever thread sets it and whenever it does so, and DFS reaches both
// the schedule in which the waiter has to block and the one in which the value is already there.
#[test]
fn wait_blocks_until_set() {
    let blocked = Arc::new(Mutex::new(HashSet::new()));
    let blocked_clone = Arc::clone(&blocked);
    check_dfs(
        move || {
            let cell = Arc::new(OnceLock::new());
            let setter = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || cell.set(2).unwrap())
            };
            thread::yield_now();
            let was_empty = cell.get().is_none();
            assert_eq!(cell.wait(), &2);
            blocked_clone.lock().unwrap().insert(was_empty);
            setter.join().unwrap();
        },
        None,
    );
    assert_eq!(*blocked.lock().unwrap(), HashSet::from([false, true]));
}

// Every waiter is woken, and each one takes causality from the thread that set the value.
#[test]
fn wait_wakes_every_waiter() {
    fn wait_test() {
        let cell = Arc::new(OnceLock::new());
        let waiters = (0..3)
            .map(|_| {
                let cell = Arc::clone(&cell);
                thread::spawn(move || {
                    let setter: &usize = cell.wait();
                    // The setter's clock entry is part of ours now.
                    assert!(current::clock().get(*setter) > 0);
                })
            })
            .collect::<Vec<_>>();
        let setter = {
            let cell = Arc::clone(&cell);
            thread::spawn(move || cell.set(usize::from(thread::current().id())).unwrap())
        };
        for waiter in waiters {
            waiter.join().unwrap();
        }
        setter.join().unwrap();
    }

    check_dfs(wait_test, Some(20_000));
    check_pct(wait_test, 1_000, 3);
}

// An initializer that panics or fails leaves the waiters waiting, as std's `wait` ignores the
// poisoning; they wake when an initializer succeeds.
#[test]
fn wait_ignores_failed_initializers() {
    check_dfs(
        || {
            let cell = Arc::new(OnceLock::new());
            let waiter = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || *cell.wait())
            };

            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                cell.get_or_init(|| panic!("expected panic"));
            }));
            assert!(result.is_err());
            assert_eq!(cell.get_or_try_init(|| Err::<usize, _>(())), Err(()));
            assert_eq!(cell.get(), None);

            cell.set(3).unwrap();
            assert_eq!(waiter.join().unwrap(), 3);
        },
        None,
    );
}

// A thread waiting for a cell that nothing initializes is a deadlock, where std blocks forever.
#[test]
#[should_panic(expected = "deadlock")]
fn wait_without_initializer_deadlocks() {
    check_dfs(
        || {
            let cell = OnceLock::<usize>::new();
            cell.wait();
        },
        None,
    );
}

// A thread that is still waiting when the execution ends is unwound by its teardown, and stops
// waiting, so that a destructor that initializes the cell afterwards finds no one to wake.
#[test]
fn wait_unwound_by_teardown() {
    use std::sync::atomic::AtomicBool;

    static CELL: OnceLock<usize> = OnceLock::new();
    static WAITING: AtomicBool = AtomicBool::new(false);
    static UNWOUND_WHILE_WAITING: AtomicUsize = AtomicUsize::new(0);

    struct SetOnDrop;
    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            let _ = CELL.set(4);
        }
    }

    struct CountUnwinding;
    impl Drop for CountUnwinding {
        fn drop(&mut self) {
            if std::thread::panicking() {
                UNWOUND_WHILE_WAITING.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    UNWOUND_WHILE_WAITING.store(0, Ordering::SeqCst);
    check_random(
        || {
            WAITING.store(false, Ordering::SeqCst);
            // A detached task, blocked when the main thread finishes, which it does without
            // initializing the cell.
            drop(shuttle::future::spawn(async {
                let _count = CountUnwinding;
                WAITING.store(true, Ordering::SeqCst);
                CELL.wait();
            }));
            while !WAITING.load(Ordering::SeqCst) {
                thread::yield_now();
            }
            // A detached task that is still running when the main thread finishes, in the middle
            // of a poll. Teardown unwinds its stack after the waiter's, and the destructor
            // initializes the cell then.
            drop(shuttle::future::spawn(async {
                let _set_on_drop = SetOnDrop;
                loop {
                    thread::yield_now();
                }
            }));
        },
        20,
    );
    assert_eq!(UNWOUND_WHILE_WAITING.load(Ordering::SeqCst), 20);
}
