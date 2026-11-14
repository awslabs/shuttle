//! Tests of the model of `once_cell::sync::OnceCell`.

use shuttle::scheduler::{DfsScheduler, RandomScheduler};
use shuttle::{Runner, check_dfs, check_pct, check_random, check_uncontrolled_nondeterminism, current, thread};
use shuttle_once_cell_inner::sync::OnceCell;
use std::collections::HashSet;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[test]
fn api() {
    check_dfs(
        || {
            let cell = OnceCell::new();
            assert_eq!(cell.get(), None);
            assert_eq!(cell.get_or_try_init(|| Err::<i32, _>(())), Err(()));
            assert_eq!(cell.get(), None);
            assert_eq!(cell.try_insert(92), Ok(&92));
            assert_eq!(cell.try_insert(62), Err((&92, 62)));
            assert_eq!(cell.set(62), Err(62));
            assert_eq!(cell.get_or_init(|| unreachable!("the cell is initialized")), &92);
            assert_eq!(cell.get_or_try_init(|| -> Result<i32, ()> { unreachable!() }), Ok(&92));
            assert_eq!(cell.wait(), &92);
            // Safety: the cell is initialized, by this task.
            assert_eq!(unsafe { cell.get_unchecked() }, &92);

            let mut cell = cell;
            *cell.get_mut().unwrap() += 1;
            assert_eq!(cell.get(), Some(&93));
            assert_eq!(cell.take(), Some(93));
            assert_eq!(cell.take(), None);
            assert_eq!(cell.get(), None);
            // A cell that was taken from can be set again.
            assert_eq!(cell.set(1), Ok(()));
            assert_eq!(cell.into_inner(), Some(1));
            assert_eq!(OnceCell::<i32>::new().into_inner(), None);
        },
        None,
    );
}

// once_cell's `Debug`, `Default`, `Clone`, `From` and `PartialEq`.
#[test]
fn trait_impls() {
    check_dfs(
        || {
            let empty = OnceCell::<String>::default();
            assert_eq!(format!("{empty:?}"), "OnceCell(Uninit)");
            let full = OnceCell::from("hello".to_string());
            assert_eq!(format!("{full:?}"), r#"OnceCell("hello")"#);
            assert_eq!(format!("{full:#?}"), "OnceCell(\n    \"hello\",\n)");

            assert_eq!(empty.clone(), empty);
            assert_eq!(full.clone(), full);
            assert_ne!(empty, full);
            let set = OnceCell::new();
            set.set("hello".to_string()).unwrap();
            assert_eq!(set, full);
            assert_eq!(set.clone().into_inner(), Some("hello".to_string()));

            let mut target = OnceCell::new();
            target.clone_from(&full);
            assert_eq!(target.get().map(String::as_str), Some("hello"));
            target.clone_from(&empty);
            assert_eq!(target.get(), None);
        },
        None,
    );
}

// A cell created with a value holds it itself: it has the value in every execution, and outside of
// one too, and initializing it is not a scheduling point.
#[test]
fn with_value() {
    static CELL: OnceCell<usize> = OnceCell::with_value(7);

    assert_eq!(CELL.get(), Some(&7));
    assert_eq!(format!("{CELL:?}"), "OnceCell(7)");
    check_dfs(
        || {
            let before = current::context_switches();
            assert_eq!(CELL.get(), Some(&7));
            assert_eq!(CELL.get_or_init(|| unreachable!()), &7);
            assert_eq!(CELL.set(8), Err(8));
            assert_eq!(CELL.wait(), &7);
            assert_eq!(current::context_switches(), before);

            let mut cell = OnceCell::with_value(1);
            assert_eq!(cell.take(), Some(1));
            assert_eq!(cell.get(), None);
            assert_eq!(cell.set(2), Ok(()));
            assert_eq!(cell.get(), Some(&2));
        },
        None,
    );
}

// The scheduling points are those of `OnceLock`: two to initialize an empty cell, the acquire and
// the release of the lock that racing initializers contend on, and none for anything else.
#[test]
fn context_switches() {
    check_dfs(
        || {
            let cell = OnceCell::new();
            let start = current::context_switches();

            assert_eq!(cell.get(), None);
            assert_eq!(current::context_switches(), start);

            assert_eq!(cell.get_or_init(|| 42), &42);
            assert_eq!(current::context_switches(), start + 2);

            assert_eq!(cell.get(), Some(&42));
            assert_eq!(cell.get_or_init(|| unreachable!()), &42);
            assert_eq!(cell.get_or_try_init(|| -> Result<i32, ()> { unreachable!() }), Ok(&42));
            assert_eq!(cell.set(43), Err(43));
            assert_eq!(cell.try_insert(43), Err((&42, 43)));
            assert_eq!(cell.wait(), &42);
            assert_eq!(current::context_switches(), start + 2);

            let mut cell = cell;
            assert_eq!(cell.get_mut(), Some(&mut 42));
            assert_eq!(cell.take(), Some(42));
            assert_eq!(current::context_switches(), start + 2);

            // Each of the other ways to initialize a cell takes the same two.
            assert_eq!(cell.set(1), Ok(()));
            assert_eq!(current::context_switches(), start + 4);
            let cell = OnceCell::new();
            assert_eq!(cell.try_insert(1), Ok(&1));
            assert_eq!(current::context_switches(), start + 6);
            let cell = OnceCell::new();
            assert_eq!(cell.get_or_try_init(|| Ok::<_, ()>(1)), Ok(&1));
            assert_eq!(current::context_switches(), start + 8);
            // A failed initialization takes them too, as it took the lock.
            let cell = OnceCell::<i32>::new();
            assert_eq!(cell.get_or_try_init(|| Err(())), Err(()));
            assert_eq!(current::context_switches(), start + 10);
        },
        None,
    );
}

// Run `num_threads` threads racing to initialize a cell, and check that exactly one initializer
// runs in each execution, that every thread sees its value, and that every thread wins in some
// execution.
fn run_race_test(num_threads: usize, tester: impl FnOnce(Box<dyn Fn() + Send + Sync>)) {
    let winners = Arc::new(Mutex::new(HashSet::new()));
    let winners_clone = Arc::clone(&winners);

    tester(Box::new(move || {
        let cell = Arc::new(OnceCell::new());
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
                    assert_eq!(cell.get(), Some(value));
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
        "every thread should win in some execution"
    );
}

#[test]
fn racing_init_dfs() {
    run_race_test(2, |f| check_dfs(f, None));
}

#[test]
fn racing_init_pct() {
    run_race_test(4, |f| check_pct(f, 1_000, 3));
}

// A `static` cell is empty again in every execution, and its initializer runs once per execution.
#[test]
fn static_resets_across_executions() {
    static CELL: OnceCell<usize> = OnceCell::new();
    static INITIALIZERS: AtomicUsize = AtomicUsize::new(0);

    let runner = Runner::new(DfsScheduler::new(None, false), Default::default());
    let executions = runner.run(|| {
        assert_eq!(
            CELL.get(),
            None,
            "nothing carried over from the execution before this one"
        );
        let threads = (0..2)
            .map(|_| {
                thread::spawn(|| {
                    let value = CELL.get_or_init(|| {
                        INITIALIZERS.fetch_add(1, Ordering::SeqCst);
                        42
                    });
                    assert_eq!(*value, 42);
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
    });

    assert!(executions > 1, "DFS should find more than one schedule");
    assert_eq!(INITIALIZERS.load(Ordering::SeqCst), executions);
}

// An initializer that panics leaves the cell empty, and the next caller runs its own, as in
// once_cell, which has no poisoning.
#[test]
fn panicking_initializer_leaves_cell_empty() {
    check_dfs(
        || {
            let cell = OnceCell::new();
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                cell.get_or_init(|| panic!("expected panic"));
            }));
            assert!(result.is_err());
            assert_eq!(cell.get(), None);
            assert_eq!(cell.get_or_init(|| 42), &42);
        },
        None,
    );
}

// A thread waiting for an initializer that fails, or panics, runs its own once the lock is free.
#[test]
fn failed_initializer_lets_a_waiting_thread_initialize() {
    let outcomes = Arc::new(Mutex::new(HashSet::new()));
    let outcomes_clone = Arc::clone(&outcomes);
    check_dfs(
        move || {
            let cell = Arc::new(OnceCell::new());
            let other = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || {
                    // Runs its initializer, which panics, unless this thread finds the cell set.
                    panic::catch_unwind(AssertUnwindSafe(|| {
                        *cell.get_or_init(|| {
                            thread::yield_now();
                            panic!("expected panic")
                        })
                    }))
                    .ok()
                })
            };
            let failed = cell.get_or_try_init(|| {
                thread::yield_now();
                Err(())
            });
            assert_eq!(failed, Err(()));
            assert_eq!(cell.get_or_init(|| 2), &2);
            outcomes_clone.lock().unwrap().insert(other.join().unwrap());
        },
        None,
    );
    // The other thread's initializer ran in some executions, and it found the cell set in others.
    assert_eq!(*outcomes.lock().unwrap(), HashSet::from([None, Some(2)]));
}

// Initializing a cell from its own initializer deadlocks in once_cell; Shuttle reports it.
#[test]
#[should_panic(expected = "deadlock")]
fn reentrant_init() {
    check_dfs(
        || {
            let cell = OnceCell::new();
            cell.get_or_init(|| *cell.get_or_init(|| 1));
        },
        None,
    );
}

#[test]
fn wait_for_another_thread() {
    let blocked = Arc::new(Mutex::new(HashSet::new()));
    let blocked_clone = Arc::clone(&blocked);
    check_dfs(
        move || {
            let cell = Arc::new(OnceCell::new());
            let setter = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || cell.set(92).unwrap())
            };
            thread::yield_now();
            let was_empty = cell.get().is_none();
            assert_eq!(cell.wait(), &92);
            blocked_clone.lock().unwrap().insert(was_empty);
            setter.join().unwrap();
        },
        None,
    );
    assert_eq!(*blocked.lock().unwrap(), HashSet::from([false, true]));
}

#[test]
#[should_panic(expected = "deadlock")]
fn wait_without_initializer_deadlocks() {
    check_dfs(
        || {
            OnceCell::<i32>::new().wait();
        },
        None,
    );
}

// A value read from a cell happens after the initializer that stored it, so a thread that sees it
// sees the initializer's other effects.
#[test]
fn reads_take_causality_from_the_initializer() {
    let saw_value = Arc::new(AtomicUsize::new(0));
    let saw_value_clone = Arc::clone(&saw_value);
    check_dfs(
        move || {
            let cell = Arc::new(OnceCell::new());
            let initializer = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || {
                    cell.set(usize::from(thread::current().id())).unwrap();
                })
            };
            thread::yield_now();
            if let Some(&id) = cell.get() {
                assert!(
                    current::clock().get(id) > 0,
                    "the reader's clock includes the initializer's"
                );
                saw_value_clone.fetch_add(1, Ordering::SeqCst);
            }
            initializer.join().unwrap();
        },
        None,
    );
    assert!(saw_value.load(Ordering::SeqCst) > 0);
}

#[test]
#[should_panic(expected = "`OnceCell::get_unchecked` was called on an empty cell")]
fn get_unchecked_on_an_empty_cell_panics() {
    check_dfs(
        || {
            // Safety: none; the model catches the mistake.
            unsafe { OnceCell::<i32>::new().get_unchecked() };
        },
        None,
    );
}

#[test]
#[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
fn empty_cell_outside_of_shuttle_panics() {
    OnceCell::<i32>::new().get();
}

// Replaying a seed reproduces the same sequence of events, which it cannot do if a cell keeps
// anything from one execution to the next.
#[test]
fn replay_from_seed() {
    static CELL: OnceCell<usize> = OnceCell::new();

    fn run(seed: u64) -> Vec<(usize, usize)> {
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_clone = Arc::clone(&log);
        let runner = Runner::new(RandomScheduler::new_from_seed(seed, 50), Default::default());
        runner.run(move || {
            let threads = (0..3)
                .map(|i| {
                    let log = Arc::clone(&log);
                    thread::spawn(move || {
                        let value = *CELL.get_or_init(|| i);
                        log.lock().unwrap().push((i, value));
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
    assert_eq!(first, run(0x5EED));
    assert_ne!(first, run(0xD1FF));
}

#[test]
fn uncontrolled_nondeterminism() {
    static CELL: OnceCell<usize> = OnceCell::new();
    check_uncontrolled_nondeterminism(
        || {
            let threads = (0..3)
                .map(|i| thread::spawn(move || *CELL.get_or_init(|| i)))
                .collect::<Vec<_>>();
            let values = threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<HashSet<_>>();
            assert_eq!(values.len(), 1);
        },
        100,
    );
}

// The same check fails for once_cell's own cell, whose value outlives the execution that stored it:
// the replay of the first schedule finds the cell set and takes a different path.
#[test]
#[should_panic(expected = "possible nondeterminism")]
fn uncontrolled_nondeterminism_with_once_cell_itself() {
    static CELL: once_cell_orig::sync::OnceCell<()> = once_cell_orig::sync::OnceCell::new();
    check_uncontrolled_nondeterminism(
        || {
            if CELL.set(()).is_ok() {
                thread::yield_now();
            }
        },
        10,
    );
}

// Concurrent Shuttle tests see their own version of a `static` cell. Best effort: it runs tests on
// several OS threads at once and hopes they overlap.
#[test]
fn shared_static() {
    static CELL: OnceCell<usize> = OnceCell::new();
    static INITIALIZERS: AtomicUsize = AtomicUsize::new(0);

    let mut executions = 0;
    for _ in 0..20 {
        let threads = (0..3)
            .map(|_| {
                std::thread::spawn(|| {
                    let runner = Runner::new(DfsScheduler::new(None, false), Default::default());
                    runner.run(|| {
                        let threads = (0..2)
                            .map(|_| {
                                thread::spawn(|| {
                                    CELL.get_or_init(|| {
                                        INITIALIZERS.fetch_add(1, Ordering::SeqCst);
                                        42
                                    });
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
        executions += threads.into_iter().map(|t| t.join().unwrap()).sum::<usize>();
    }
    assert_eq!(executions, INITIALIZERS.load(Ordering::SeqCst));
}

// Same seed, same behaviour, for a test that mixes the cell with other shared state.
#[test]
fn same_seed_same_behaviour() {
    fn observations(seed: u64) -> Vec<Vec<usize>> {
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_clone = Arc::clone(&log);
        shuttle::check_random_with_seed(
            move || {
                let cell = Arc::new(OnceCell::new());
                let order = Arc::new(shuttle::sync::Mutex::new(Vec::new()));
                let threads = (0..3)
                    .map(|i| {
                        let cell = Arc::clone(&cell);
                        let order = Arc::clone(&order);
                        thread::spawn(move || {
                            let winner = *cell.get_or_init(|| i);
                            order.lock().unwrap().push(winner * 10 + i);
                        })
                    })
                    .collect::<Vec<_>>();
                for thread in threads {
                    thread.join().unwrap();
                }
                log_clone.lock().unwrap().push(order.lock().unwrap().clone());
            },
            seed,
            30,
        );
        Arc::try_unwrap(log).unwrap().into_inner().unwrap()
    }
    assert_eq!(observations(7), observations(7));
    assert_ne!(observations(7), observations(8));
}

#[test]
fn many_threads_random() {
    check_random(
        || {
            let cell = Arc::new(OnceCell::new());
            let threads = (0..8)
                .map(|i| {
                    let cell = Arc::clone(&cell);
                    thread::spawn(move || {
                        if i % 2 == 0 {
                            *cell.get_or_init(|| i)
                        } else {
                            *cell.wait()
                        }
                    })
                })
                .collect::<Vec<_>>();
            let values = threads.into_iter().map(|t| t.join().unwrap()).collect::<HashSet<_>>();
            assert_eq!(values.len(), 1);
            assert_eq!(values.into_iter().next().unwrap() % 2, 0);
        },
        200,
    );
}
