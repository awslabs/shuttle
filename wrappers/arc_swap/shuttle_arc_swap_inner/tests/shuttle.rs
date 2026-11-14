//! How the model behaves across tasks: linearizability, what a `Guard` keeps alive, replay, and
//! the state of a `static` in each execution. Also what the real arc-swap does under Shuttle,
//! where that is cheap to show.

use shuttle::sync::Arc as ShuttleArc;
use shuttle::sync::atomic::{AtomicUsize, Ordering};
use shuttle::{check_dfs, check_random, check_random_with_seed, check_uncontrolled_nondeterminism, thread};
use shuttle_arc_swap_inner::{ArcSwap, ArcSwapOption};
use std::collections::HashSet;
use std::sync::Arc;

/// A seeded random check, for the scenarios whose task count makes an exhaustive one too large.
/// The seed keeps the run reproducible.
fn check_random_with_seed_and_count(test: impl Fn() + Send + Sync + 'static) {
    check_random_with_seed(test, 0x5EED, 500);
}

/// Collects what each execution produced, for a check that runs executions.
fn collected<T: Send + 'static>(run: impl FnOnce(Arc<std::sync::Mutex<Vec<T>>>)) -> Vec<T> {
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    run(log.clone());
    std::mem::take(&mut *log.lock().unwrap())
}

/// A load sees either the old value or the new one, never anything else: the operations are
/// linearizable.
#[test]
fn a_load_sees_one_of_the_stored_values() {
    let seen: HashSet<u32> = collected(|log| {
        check_dfs(
            move || {
                let storage = Arc::new(ArcSwap::from_pointee(0u32));
                let writer = {
                    let storage = storage.clone();
                    thread::spawn(move || storage.store(Arc::new(1)))
                };
                let read = **storage.load();
                writer.join().unwrap();
                log.lock().unwrap().push(read);
            },
            None,
        );
    })
    .into_iter()
    .collect();
    assert_eq!(seen, HashSet::from([0, 1]), "both the old and the new value are seen");
}

/// Two writers and a reader: the reader sees one of the three values, and the last store wins.
#[test]
fn stores_are_atomic() {
    let seen: HashSet<(u32, u32)> = collected(|log| {
        check_random_with_seed_and_count(move || {
            let storage = Arc::new(ArcSwap::from_pointee(0u32));
            let writers: Vec<_> = (1..3)
                .map(|i| {
                    let storage = storage.clone();
                    thread::spawn(move || storage.store(Arc::new(i)))
                })
                .collect();
            let read = **storage.load();
            for writer in writers {
                writer.join().unwrap();
            }
            let end = **storage.load();
            log.lock().unwrap().push((read, end));
        });
    })
    .into_iter()
    .collect();
    for (read, end) in &seen {
        assert!([0, 1, 2].contains(read), "a stored value, not a mix: {read}");
        assert!([1, 2].contains(end), "one of the writers won: {end}");
    }
    assert!(seen.len() > 1, "the schedules differ: {seen:?}");
}

/// A `swap` returns exactly one of the values that was in the storage, and every value is returned
/// to exactly one task: nothing is lost or handed out twice.
#[test]
fn every_swap_takes_a_distinct_value() {
    check_random_with_seed_and_count(|| {
        let storage = Arc::new(ArcSwap::from_pointee(0u32));
        let swappers: Vec<_> = (1..3)
            .map(|i| {
                let storage = storage.clone();
                thread::spawn(move || *storage.swap(Arc::new(i)))
            })
            .collect();
        let mut taken: Vec<u32> = swappers.into_iter().map(|t| t.join().unwrap()).collect();
        taken.push(**storage.load());
        taken.sort_unstable();
        assert_eq!(taken, [0, 1, 2], "each value came out once");
    });
}

/// `compare_and_swap` is atomic: of two tasks that race to replace the same value, exactly one
/// succeeds.
#[test]
fn compare_and_swap_has_one_winner() {
    check_random_with_seed_and_count(|| {
        let storage = Arc::new(ArcSwap::from_pointee(0u32));
        let start = storage.load_full();
        let winners = ShuttleArc::new(AtomicUsize::new(0));
        let tasks: Vec<_> = (1..3)
            .map(|i| {
                let storage = storage.clone();
                let start = start.clone();
                let winners = winners.clone();
                thread::spawn(move || {
                    let previous = storage.compare_and_swap(&start, Arc::new(i));
                    if Arc::ptr_eq(&previous, &start) {
                        winners.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for task in tasks {
            task.join().unwrap();
        }
        assert_eq!(winners.load(Ordering::SeqCst), 1, "exactly one exchange happened");
        assert_ne!(**storage.load(), 0, "and it took effect");
    });
}

/// `rcu` loses no update: two tasks that each add one to a counter end at two, whatever the
/// schedule, because the loser's `compare_and_swap` fails and its closure runs again.
#[test]
fn rcu_loses_no_update() {
    check_random_with_seed_and_count(|| {
        let storage = Arc::new(ArcSwap::from_pointee(0u32));
        let tasks: Vec<_> = (0..2)
            .map(|_| {
                let storage = storage.clone();
                thread::spawn(move || storage.rcu(|old| Arc::new(**old + 1)))
            })
            .collect();
        for task in tasks {
            task.join().unwrap();
        }
        assert_eq!(**storage.load(), 2);
    });
}

/// A `Guard` keeps the value it loaded alive, whatever the other tasks store, and the value is
/// freed once the last guard is gone.
#[test]
fn a_guard_keeps_the_old_value_alive() {
    /// Counts the live values of the executions, so it is a real atomic.
    static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    struct Counted(u32);

    impl Counted {
        fn new(value: u32) -> Self {
            LIVE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Self(value)
        }
    }

    impl Drop for Counted {
        fn drop(&mut self) {
            LIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    check_random_with_seed_and_count(|| {
        let storage = Arc::new(ArcSwap::from_pointee(Counted::new(0)));
        let guard = storage.load();
        let writers: Vec<_> = (1..3)
            .map(|i| {
                let storage = storage.clone();
                thread::spawn(move || storage.store(Arc::new(Counted::new(i))))
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        // Both writers have replaced the value, so no `load` can reach the one the guard
        // holds; the guard's own handle keeps it alive and readable.
        assert_eq!(guard.0, 0, "no use after free");
        drop(guard);
        // What is left is the value in the storage, and the storage's initial value, which it
        // keeps a handle on so that every execution starts from it (see the crate
        // documentation). The value the first writer stored, which the second replaced, is
        // gone.
        assert_eq!(LIVE.load(std::sync::atomic::Ordering::SeqCst), 2);
    });
    assert_eq!(
        LIVE.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "every value was freed by the end"
    );
}

/// A `Cache` holds the value it last loaded, so that value is not freed under it either.
#[test]
fn a_cache_keeps_its_value_alive() {
    use shuttle_arc_swap_inner::Cache;

    check_dfs(
        || {
            let storage = Arc::new(ArcSwap::from_pointee(0u32));
            let mut cache = Cache::new(storage.clone());
            assert_eq!(**cache.load(), 0);
            let writer = {
                let storage = storage.clone();
                thread::spawn(move || storage.store(Arc::new(1)))
            };
            thread::yield_now();
            // The cache revalidates, so it sees whichever value is in the storage now.
            assert!([0, 1].contains(&**cache.load()));
            writer.join().unwrap();
            assert_eq!(**cache.load(), 1);
        },
        None,
    );
}

// The one way to have an `ArcSwapAny` in a `static` without a `Lazy` or a `OnceLock`.
static STATIC_EMPTY: ArcSwapOption<u32> = ArcSwapOption::const_empty();
/// Counts the executions, which a `'static` closure cannot borrow a local for.
static EXECUTIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[test]
fn a_const_constructed_static_resets_across_executions() {
    EXECUTIONS.store(0, std::sync::atomic::Ordering::SeqCst);
    check_random(
        || {
            EXECUTIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert!(STATIC_EMPTY.load().is_none(), "empty at the start of the execution");
            STATIC_EMPTY.store(Some(Arc::new(1)));
            assert_eq!(**STATIC_EMPTY.load().as_ref().unwrap(), 1);
            // A task of this execution sees the value too.
            let other = thread::spawn(|| STATIC_EMPTY.load().as_deref().copied());
            assert_eq!(other.join().unwrap(), Some(1));
        },
        20,
    );
    assert_eq!(EXECUTIONS.load(std::sync::atomic::Ordering::SeqCst), 20);
}

/// Three OS threads running Shuttle tests on the same `static`, as upstream's `once.rs` and
/// `lazy_static.rs` tests do: one execution's state must not be another's.
#[test]
fn shared_static() {
    static SHARED: ArcSwapOption<u32> = ArcSwapOption::const_empty();

    let threads: Vec<_> = (0..3)
        .map(|_| {
            std::thread::spawn(|| {
                check_random(
                    || {
                        assert!(SHARED.load().is_none());
                        SHARED.store(Some(Arc::new(7)));
                        assert_eq!(**SHARED.load().as_ref().unwrap(), 7);
                    },
                    30,
                )
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn same_seed_same_behaviour() {
    fn run(seed: u64) -> Vec<Vec<u32>> {
        collected(|log| {
            check_random_with_seed(
                move || {
                    let storage = Arc::new(ArcSwap::from_pointee(0u32));
                    let reads = Arc::new(std::sync::Mutex::new(Vec::new()));
                    let tasks: Vec<_> = (1..4)
                        .map(|i| {
                            let storage = storage.clone();
                            let reads = reads.clone();
                            thread::spawn(move || {
                                if i == 1 {
                                    storage.store(Arc::new(i));
                                } else {
                                    // The load has a scheduling point, so it must not run while
                                    // this task holds the real `std` mutex below: another task
                                    // that took it would block the one OS thread the execution
                                    // runs on.
                                    let read = **storage.load();
                                    reads.lock().unwrap().push(read);
                                    storage.rcu(|old| Arc::new(**old + 10));
                                }
                            })
                        })
                        .collect();
                    for task in tasks {
                        task.join().unwrap();
                    }
                    let mut reads = reads.lock().unwrap().clone();
                    reads.push(**storage.load());
                    log.lock().unwrap().push(reads);
                },
                seed,
                30,
            );
        })
    }
    let first = run(0x5EED);
    assert_eq!(first, run(0x5EED), "the same seed gives the same executions");
    assert_ne!(first, run(0xD1FF), "another seed gives others");
}

#[test]
fn nondeterminism_check_passes() {
    check_uncontrolled_nondeterminism(
        || {
            let storage = Arc::new(ArcSwap::from_pointee(0u32));
            let option = Arc::new(ArcSwapOption::<u32>::empty());
            let tasks: Vec<_> = (1..3)
                .map(|i| {
                    let storage = storage.clone();
                    let option = option.clone();
                    thread::spawn(move || {
                        let seen = **storage.load();
                        storage.store(Arc::new(seen + i));
                        option.rcu(|old| old.as_ref().map(|value| Arc::new(**value + i)).or(Some(Arc::new(i))));
                    })
                })
                .collect();
            for task in tasks {
                task.join().unwrap();
            }
        },
        50,
    );
}

/// What the real arc-swap does under Shuttle, for comparison.
mod real_arc_swap {
    use super::*;

    /// The real crate's `load` and `store` take no scheduling point, so Shuttle never runs another
    /// task around them and misses the interleavings the model explores
    /// (`a_load_sees_one_of_the_stored_values`).
    #[test]
    fn real_operations_are_not_scheduling_points() {
        check_dfs(
            || {
                let storage = arc_swap_orig::ArcSwap::from_pointee(0u32);
                let before = shuttle::current::context_switches();
                let _ = storage.load();
                storage.store(Arc::new(1));
                let _ = storage.swap(Arc::new(2));
                assert_eq!(shuttle::current::context_switches(), before);
            },
            None,
        );
    }

    /// With the real crate, a `static` keeps what the previous execution stored, so an execution
    /// depends on the ones before it and a failing schedule cannot be replayed on its own.
    #[test]
    fn a_real_static_leaks_across_executions() {
        static REAL: arc_swap_orig::ArcSwapOption<u32> = arc_swap_orig::ArcSwapOption::const_empty();
        static EMPTY_AT_START: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        static EXECUTIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        check_random(
            || {
                EXECUTIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if REAL.load().is_none() {
                    EMPTY_AT_START.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                REAL.store(Some(Arc::new(1)));
            },
            10,
        );
        assert_eq!(EXECUTIONS.load(std::sync::atomic::Ordering::SeqCst), 10);
        assert_eq!(
            EMPTY_AT_START.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "only the first of the ten executions found the static empty, where every execution \
             finds the model's empty"
        );
    }
}
