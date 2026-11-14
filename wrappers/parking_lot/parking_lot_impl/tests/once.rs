//! Shuttle's model of `parking_lot::Once`.

use shuttle::scheduler::{DfsScheduler, RandomScheduler};
use shuttle::sync::atomic::{AtomicUsize, Ordering};
use shuttle::{Runner, check_dfs, check_random, check_uncontrolled_nondeterminism, current, thread};
use shuttle_parking_lot_impl::{Once, OnceState};
use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

#[test]
fn runs_one_closure() {
    check_dfs(
        || {
            let once = Once::new();
            assert_eq!(once.state(), OnceState::New);
            let mut runs = 0;
            once.call_once(|| runs += 1);
            once.call_once(|| runs += 1);
            once.call_once_force(|_| runs += 1);
            assert_eq!(runs, 1);
            assert_eq!(once.state(), OnceState::Done);
            assert!(once.state().done() && !once.state().poisoned());
        },
        None,
    );
}

/// Racing callers: one closure runs, the others wait for it, and every caller wins in some
/// execution.
#[test]
fn racing_callers() {
    let winners = Arc::new(std::sync::Mutex::new(BTreeSet::new()));
    let winners_clone = Arc::clone(&winners);
    check_dfs(
        move || {
            let once = Arc::new(Once::new());
            let runs = Arc::new(AtomicUsize::new(0));
            let callers = (0..2)
                .map(|i| {
                    let (once, runs, winners) = (Arc::clone(&once), Arc::clone(&runs), Arc::clone(&winners_clone));
                    thread::spawn(move || {
                        once.call_once(|| {
                            runs.fetch_add(1, Ordering::SeqCst);
                            winners.lock().unwrap().insert(i);
                        });
                        // Whoever ran it, it is done when `call_once` returns.
                        assert_eq!(runs.load(Ordering::SeqCst), 1);
                        assert!(once.state().done());
                    })
                })
                .collect::<Vec<_>>();
            for caller in callers {
                caller.join().unwrap();
            }
        },
        None,
    );
    assert_eq!(*winners.lock().unwrap(), BTreeSet::from([0, 1]));
}

/// Another task sees the closure in progress.
#[test]
fn in_progress_is_visible() {
    let seen = Arc::new(std::sync::Mutex::new(BTreeSet::new()));
    let seen_clone = Arc::clone(&seen);
    check_dfs(
        move || {
            let once = Arc::new(Once::new());
            let observer = {
                let (once, seen) = (Arc::clone(&once), Arc::clone(&seen_clone));
                thread::spawn(move || seen.lock().unwrap().insert(format!("{:?}", once.state())))
            };
            once.call_once(thread::yield_now);
            observer.join().unwrap();
        },
        None,
    );
    let seen = seen.lock().unwrap();
    assert!(
        seen.contains("New") && seen.contains("InProgress") && seen.contains("Done"),
        "{seen:?}"
    );
}

/// A closure that panics poisons the `Once`: `call_once` then panics with `parking_lot`'s message,
/// and `call_once_force` runs, told about the poison, and clears it.
#[test]
fn poison() {
    check_dfs(
        || {
            let once = Once::new();
            let panicked = catch_unwind(AssertUnwindSafe(|| once.call_once(|| panic!("the closure panicked"))));
            assert!(panicked.is_err());
            assert_eq!(once.state(), OnceState::Poisoned);
            assert!(once.state().poisoned());

            let again = catch_unwind(AssertUnwindSafe(|| once.call_once(|| unreachable!())));
            let message = again.unwrap_err();
            assert_eq!(
                message.downcast_ref::<&str>().copied(),
                Some("Once instance has previously been poisoned")
            );

            // A forced closure that panics leaves it poisoned.
            let forced = catch_unwind(AssertUnwindSafe(|| {
                once.call_once_force(|state| {
                    assert_eq!(state, OnceState::Poisoned);
                    panic!("again")
                })
            }));
            assert!(forced.is_err());
            assert_eq!(once.state(), OnceState::Poisoned);

            let mut told = None;
            once.call_once_force(|state| told = Some(state));
            assert_eq!(told, Some(OnceState::Poisoned));
            assert_eq!(once.state(), OnceState::Done);
            once.call_once(|| unreachable!());

            let fresh = Once::new();
            fresh.call_once_force(|state| assert_eq!(state, OnceState::New));
        },
        None,
    );
}

/// The callers waiting while a closure panics: a `call_once` panics if it looks while the `Once` is
/// poisoned, and a `call_once_force` runs its own closure. Exactly one closure ever completes.
#[test]
fn waiters_of_a_closure_that_panics() {
    let outcomes = Arc::new(std::sync::Mutex::new(BTreeSet::new()));
    let outcomes_clone = Arc::clone(&outcomes);
    check_dfs(
        move || {
            let once = Arc::new(Once::new());
            // How many closures completed, which must come to exactly one.
            let completed = Arc::new(AtomicUsize::new(0));
            let first = {
                let once = Arc::clone(&once);
                thread::spawn(move || {
                    catch_unwind(AssertUnwindSafe(|| {
                        once.call_once(|| {
                            thread::yield_now();
                            panic!("the first closure panicked");
                        })
                    }))
                    .is_err()
                })
            };
            let plain = {
                let (once, completed) = (Arc::clone(&once), Arc::clone(&completed));
                thread::spawn(move || {
                    catch_unwind(AssertUnwindSafe(|| {
                        once.call_once(|| _ = completed.fetch_add(1, Ordering::SeqCst))
                    }))
                    .is_err()
                })
            };
            let forced = {
                let (once, completed) = (Arc::clone(&once), Arc::clone(&completed));
                thread::spawn(move || once.call_once_force(|_| _ = completed.fetch_add(1, Ordering::SeqCst)))
            };
            let first_panicked = first.join().unwrap();
            let plain_panicked = plain.join().unwrap();
            forced.join().unwrap();
            assert!(once.state().done());
            assert_eq!(completed.load(Ordering::SeqCst), 1, "more than one closure completed");
            // A `call_once` panics only because the first closure poisoned the `Once`.
            assert!(!plain_panicked || first_panicked);
            outcomes_clone.lock().unwrap().insert((first_panicked, plain_panicked));
        },
        None,
    );
    // The first closure panics unless another completed first, and the plain call panics in some
    // execution and not in others.
    assert_eq!(
        *outcomes.lock().unwrap(),
        BTreeSet::from([(false, false), (true, false), (true, true)])
    );
}

/// A closure that calls the same `Once` waits for itself, which Shuttle reports as a deadlock
/// (`parking_lot` deadlocks).
#[test]
#[should_panic(expected = "deadlock")]
fn a_reentrant_call_deadlocks() {
    check_dfs(
        || {
            let once = Once::new();
            once.call_once(|| once.call_once(|| {}));
        },
        None,
    );
}

/// No scheduling point once a closure completed; two on the way in (take and release the lock);
/// one for `state`.
#[test]
fn context_switches() {
    check_dfs(
        || {
            let once = Once::new();
            let count = |f: &dyn Fn()| {
                let before = current::context_switches();
                f();
                current::context_switches() - before
            };
            assert_eq!(count(&|| once.call_once(|| {})), 2);
            assert_eq!(count(&|| once.call_once(|| {})), 0);
            assert_eq!(count(&|| once.call_once_force(|_| {})), 0);
            assert_eq!(count(&|| _ = once.state()), 1);
        },
        None,
    );
}

/// What the closure did happens before what a caller does after `call_once` returns, even a caller
/// that found it done without waiting.
#[test]
fn the_closure_happens_before_the_callers() {
    check_dfs(
        || {
            let once = Arc::new(Once::new());
            let value = Arc::new(AtomicUsize::new(0));
            let callers = (0..2)
                .map(|_| {
                    let (once, value) = (Arc::clone(&once), Arc::clone(&value));
                    thread::spawn(move || {
                        once.call_once(|| value.store(42, Ordering::Relaxed));
                        assert_eq!(value.load(Ordering::Relaxed), 42);
                    })
                })
                .collect::<Vec<_>>();
            for caller in callers {
                caller.join().unwrap();
            }
        },
        None,
    );
}

#[test]
fn debug() {
    check_dfs(
        || {
            let once = Once::new();
            assert_eq!(format!("{once:?}"), "Once { state: New }");
            once.call_once(|| {});
            assert_eq!(format!("{once:?}"), "Once { state: Done }");
            assert_eq!(format!("{:?}", Once::default()), "Once { state: New }");
        },
        None,
    );
}

fn race_log() -> Vec<usize> {
    let once = Arc::new(Once::new());
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let callers = (0..3)
        .map(|i| {
            let (once, log) = (Arc::clone(&once), Arc::clone(&log));
            thread::spawn(move || once.call_once(|| log.lock().unwrap().push(i)))
        })
        .collect::<Vec<_>>();
    for caller in callers {
        caller.join().unwrap();
    }
    let log = log.lock().unwrap();
    log.clone()
}

#[test]
fn replays_from_a_seed() {
    let run = |seed| {
        let logs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let logs_clone = Arc::clone(&logs);
        Runner::new(RandomScheduler::new_from_seed(seed, 50), Default::default())
            .run(move || logs_clone.lock().unwrap().push(race_log()));
        Arc::try_unwrap(logs).unwrap().into_inner().unwrap()
    };
    let first = run(0x5EED);
    assert_eq!(first, run(0x5EED));
    assert_ne!(first, run(0xD1FF));
}

#[test]
fn uncontrolled_nondeterminism() {
    check_uncontrolled_nondeterminism(
        || {
            let _ = race_log();
        },
        100,
    );
}

#[test]
fn random_scheduler() {
    check_random(
        || {
            assert_eq!(race_log().len(), 1);
        },
        500,
    );
    let executions = Runner::new(DfsScheduler::new(None, false), Default::default()).run(|| {
        assert_eq!(race_log().len(), 1);
    });
    assert!(executions > 1);
}
