//! Tests for `sync::SetOnce`.

use futures::FutureExt;
use shuttle::sync::atomic::AtomicBool;
use shuttle::{check_dfs, check_random_with_seed, check_uncontrolled_nondeterminism, future, thread};
use shuttle_tokio_impl_inner::sync::{SetOnce, SetOnceError};
use std::collections::BTreeSet;
use std::error::Error;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use test_log::test;

/// Runs `f` once, in a Shuttle test with a single task.
fn in_a_shuttle_test(f: impl Fn() + Send + Sync + 'static) {
    check_dfs(f, None);
}

#[test]
fn api() {
    in_a_shuttle_test(|| {
        let cell = SetOnce::new();
        assert!(!cell.initialized());
        assert_eq!(cell.get(), None);
        assert_eq!(cell.set(1), Ok(()));
        assert!(cell.initialized());
        assert_eq!(cell.get(), Some(&1));
        // A second `set` fails, and hands its value back.
        assert_eq!(cell.set(2), Err(SetOnceError(2)));
        assert_eq!(cell.get(), Some(&1));
        assert_eq!(future::block_on(cell.wait()), &1);
        assert_eq!(cell.into_inner(), Some(1));

        assert_eq!(SetOnce::<u32>::new().into_inner(), None);
        assert_eq!(SetOnce::<u32>::default().get(), None);
        assert_eq!(SetOnce::new_with(None::<u32>).get(), None);
        assert_eq!(SetOnce::new_with(Some(3)).get(), Some(&3));
        assert_eq!(SetOnce::from(4).get(), Some(&4));
        assert_eq!(SetOnce::from(4).set(5), Err(SetOnceError(5)));

        // Clone, PartialEq and Eq compare and copy the values.
        let set = SetOnce::from(6);
        assert_eq!(set.clone().get(), Some(&6));
        assert_eq!(SetOnce::<u32>::new().clone().get(), None);
        assert_eq!(set, SetOnce::from(6));
        assert_ne!(set, SetOnce::from(7));
        assert_ne!(set, SetOnce::new());
        assert_eq!(SetOnce::<u32>::new(), SetOnce::new());
    });
}

/// `SetOnce` and `SetOnceError` format, and the error displays, as tokio's do.
#[test]
fn formatting_matches_tokio() {
    in_a_shuttle_test(|| {
        assert_eq!(
            format!("{:?}", SetOnce::<u32>::new()),
            format!("{:?}", tokio::sync::SetOnce::<u32>::new())
        );
        assert_eq!(
            format!("{:?}", SetOnce::from(1)),
            format!("{:?}", tokio::sync::SetOnce::from(1))
        );
        assert_eq!(
            format!("{:?}", SetOnceError(2)),
            format!("{:?}", tokio::sync::SetOnceError(2))
        );
        assert_eq!(SetOnceError(2).to_string(), tokio::sync::SetOnceError(2).to_string());
        let error: &dyn Error = &SetOnceError(2);
        assert!(error.source().is_none());
    });
}

/// The `const` constructors work in a `static`, and its reads and writes work outside of a Shuttle
/// test too, as tokio's do: there is no task to switch to there, so they take no scheduling point.
#[test]
fn in_a_static_outside_of_a_shuttle_test() {
    static EMPTY: SetOnce<u32> = SetOnce::const_new();
    static FULL: SetOnce<u32> = SetOnce::const_new_with(1);

    assert!(!EMPTY.initialized());
    assert_eq!(EMPTY.get(), None);
    assert_eq!(format!("{EMPTY:?}"), "SetOnce { value: None }");
    assert_eq!(EMPTY.set(2), Ok(()));
    assert_eq!(EMPTY.get(), Some(&2));
    assert_eq!(EMPTY.set(3), Err(SetOnceError(3)));
    // The value is set, so the wait returns at once, without the semaphore.
    assert_eq!(EMPTY.wait().now_or_never(), Some(&2));

    assert!(FULL.initialized());
    assert_eq!(FULL.get(), Some(&1));
    assert_eq!(FULL.set(4), Err(SetOnceError(4)));
    assert_eq!(FULL.wait().now_or_never(), Some(&1));
}

/// A task that waits gets the value another task sets, whichever of the two comes first, and
/// Shuttle explores both.
#[test]
fn wait_returns_the_value_set_by_another_task() {
    let orders = Arc::new(Mutex::new(BTreeSet::new()));
    let seen = orders.clone();

    check_dfs(
        move || {
            let seen = seen.clone();
            future::block_on(async move {
                let cell = Arc::new(SetOnce::new());
                let setter = {
                    let cell = cell.clone();
                    future::spawn(async move { cell.set(1).unwrap() })
                };
                let set_before_the_wait = cell.initialized();
                assert_eq!(cell.wait().await, &1);
                seen.lock().unwrap().insert(set_before_the_wait);
                setter.await.unwrap();
            })
        },
        None,
    );

    assert_eq!(*orders.lock().unwrap(), BTreeSet::from([false, true]));
}

/// Every task that waits is woken by the one `set`.
#[test]
fn every_waiter_is_woken() {
    check_dfs(
        || {
            future::block_on(async {
                let cell = Arc::new(SetOnce::new());
                let waiters = (0..2)
                    .map(|_| {
                        let cell = cell.clone();
                        future::spawn(async move { *cell.wait().await })
                    })
                    .collect::<Vec<_>>();
                cell.set(1).unwrap();
                for waiter in waiters {
                    assert_eq!(waiter.await.unwrap(), 1);
                }
            })
        },
        None,
    );
}

/// Of two concurrent `set`s exactly one succeeds, the other gets its own value back, and Shuttle
/// lets either one win.
#[test]
fn exactly_one_set_succeeds() {
    let winners = Arc::new(Mutex::new(BTreeSet::new()));
    let seen = winners.clone();

    check_dfs(
        move || {
            let cell = Arc::new(SetOnce::new());
            let setters = (1..=2)
                .map(|value| {
                    let cell = cell.clone();
                    thread::spawn(move || cell.set(value).map(|()| value))
                })
                .collect::<Vec<_>>();
            let results = setters.into_iter().map(|t| t.join().unwrap()).collect::<Vec<_>>();

            let won = results
                .iter()
                .filter_map(|r| r.as_ref().ok())
                .copied()
                .collect::<Vec<_>>();
            assert_eq!(won.len(), 1, "{results:?}");
            let lost = results.iter().filter_map(|r| r.as_ref().err()).collect::<Vec<_>>();
            assert_eq!(lost, [&SetOnceError(3 - won[0])], "the loser gets its own value back");
            assert_eq!(cell.get(), Some(&won[0]));
            seen.lock().unwrap().insert(won[0]);
        },
        None,
    );

    assert_eq!(*winners.lock().unwrap(), BTreeSet::from([1, 2]));
}

/// Regression test: Shuttle must be able to schedule another task between the setter's previous
/// operation and the store of the value.
///
/// The test program wrongly assumes that once it sees `ready`, which the setter stores right before
/// it sets the cell, the cell has a value. That fails when the other task runs between the two,
/// which Shuttle only explores if `set` takes a scheduling point before its store.
#[test]
#[should_panic(expected = "value not stored yet")]
fn set_takes_a_scheduling_point_before_the_store() {
    check_dfs(
        || {
            let cell = Arc::new(SetOnce::new());
            let ready = Arc::new(AtomicBool::new(false));
            let reader = {
                let (cell, ready) = (cell.clone(), ready.clone());
                thread::spawn(move || {
                    if ready.load(Ordering::SeqCst) {
                        // BUG (in this test program): assumes the value has already been stored.
                        assert!(cell.get().is_some(), "value not stored yet");
                    }
                })
            };
            ready.store(true, Ordering::SeqCst);
            cell.set(1).unwrap();
            reader.join().unwrap();
        },
        None,
    );
}

/// Regression test: Shuttle must be able to schedule another task between the reader's previous
/// operation and its read of the cell.
///
/// The test program wrongly assumes that the cell is still empty right after the reader stores
/// `asked`, as the setter only sets it once it sees `asked`. That fails when the setter runs
/// between the two, which Shuttle only explores if `get` takes a scheduling point before its read.
#[test]
#[should_panic(expected = "set between the store and the read")]
fn get_takes_a_scheduling_point_before_the_read() {
    check_dfs(
        || {
            let cell = Arc::new(SetOnce::new());
            let asked = Arc::new(AtomicBool::new(false));
            let setter = {
                let (cell, asked) = (cell.clone(), asked.clone());
                thread::spawn(move || {
                    if asked.load(Ordering::SeqCst) {
                        cell.set(1).unwrap();
                    }
                })
            };
            asked.store(true, Ordering::SeqCst);
            // BUG (in this test program): assumes nothing runs between the store and the read.
            assert!(cell.get().is_none(), "set between the store and the read");
            setter.join().unwrap();
        },
        None,
    );
}

/// A `wait` dropped while it is queued leaves the queue: the `set` after it wakes nobody stale, and
/// later waits, in this task and in another, still get the value. tokio documents `wait` as cancel
/// safe.
#[test]
fn wait_is_cancel_safe() {
    check_dfs(
        || {
            future::block_on(async {
                let cell = Arc::new(SetOnce::new());
                {
                    let mut wait = Box::pin(cell.wait());
                    assert!(wait.as_mut().now_or_never().is_none());
                }
                let waiter = {
                    let cell = cell.clone();
                    future::spawn(async move { *cell.wait().await })
                };
                cell.set(1).unwrap();
                assert_eq!(cell.wait().await, &1);
                assert_eq!(waiter.await.unwrap(), 1);
            })
        },
        None,
    );
}

/// A `wait` dropped by a `select!` whose other branch completes does not keep the task from
/// getting the value later.
#[test]
fn wait_dropped_by_select() {
    check_dfs(
        || {
            future::block_on(async {
                let cell = Arc::new(SetOnce::new());
                let (tx, mut rx) = shuttle_tokio_impl_inner::sync::mpsc::unbounded_channel::<()>();
                let selecting = {
                    let cell = cell.clone();
                    future::spawn(async move {
                        let first = shuttle_tokio_impl_inner::select! {
                            biased;
                            value = cell.wait() => Some(*value),
                            _ = rx.recv() => None,
                        };
                        (first, *cell.wait().await)
                    })
                };
                tx.send(()).unwrap();
                cell.set(1).unwrap();
                let (first, later) = selecting.await.unwrap();
                assert!(matches!(first, None | Some(1)));
                assert_eq!(later, 1);
            })
        },
        None,
    );
}

/// A cell that is never set keeps its waiters waiting forever, and Shuttle reports it.
#[test]
#[should_panic(expected = "deadlock")]
fn wait_without_set_deadlocks() {
    check_dfs(
        || {
            future::block_on(async {
                let cell = SetOnce::<u32>::new();
                cell.wait().await;
            })
        },
        None,
    );
}

/// The value is dropped exactly once: with the cell, or by whoever takes it out with `into_inner`,
/// and a value that `set` refuses is handed back rather than dropped by the cell.
#[test]
fn drops_its_value_once() {
    in_a_shuttle_test(|| {
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        DROPS.store(0, Ordering::SeqCst);

        #[derive(Debug, PartialEq)]
        struct CountsDrops;
        impl Drop for CountsDrops {
            fn drop(&mut self) {
                DROPS.fetch_add(1, Ordering::SeqCst);
            }
        }

        let cell = SetOnce::new();
        cell.set(CountsDrops).unwrap();
        let refused = cell.set(CountsDrops).unwrap_err();
        assert_eq!(DROPS.load(Ordering::SeqCst), 0);
        drop(refused);
        assert_eq!(DROPS.load(Ordering::SeqCst), 1);
        drop(cell);
        assert_eq!(DROPS.load(Ordering::SeqCst), 2);

        let cell = SetOnce::from(CountsDrops);
        let value = cell.into_inner();
        assert_eq!(DROPS.load(Ordering::SeqCst), 2);
        drop(value);
        assert_eq!(DROPS.load(Ordering::SeqCst), 3);

        drop(SetOnce::<CountsDrops>::new());
        assert_eq!(DROPS.load(Ordering::SeqCst), 3);
    });
}

/// `SetOnce` and its error are `Send` and `Sync` exactly when tokio's are, and so is the `wait`
/// future.
#[test]
fn auto_traits() {
    fn send_sync<T: Send + Sync>() {}
    fn send_sync_value<T: Send + Sync>(_: &T) {}
    send_sync::<SetOnce<u32>>();
    send_sync::<SetOnceError<u32>>();
    send_sync_value(&SetOnce::<u32>::const_new().wait());

    fn send<T: Send>() {}
    // `Cell` is `Send` and not `Sync`, so the cell is `Send` and not `Sync`, as tokio's is.
    send::<SetOnce<std::cell::Cell<u32>>>();
    send::<tokio::sync::SetOnce<std::cell::Cell<u32>>>();
}

/// `SetOnce` introduces no randomness of its own, so every schedule replays: each iteration runs a
/// random schedule and then replays it, and fails if the replay diverges.
#[test]
fn no_uncontrolled_nondeterminism() {
    check_uncontrolled_nondeterminism(
        || {
            future::block_on(async {
                let cell = Arc::new(SetOnce::new());
                let tasks = (1..=2)
                    .map(|value| {
                        let cell = cell.clone();
                        future::spawn(async move {
                            let _ = cell.set(value);
                            *cell.wait().await
                        })
                    })
                    .collect::<Vec<_>>();
                let winner = *cell.wait().await;
                for task in tasks {
                    assert_eq!(task.await.unwrap(), winner);
                }
            })
        },
        500,
    );
}

/// Two runs from the same seed take the same schedules, so the same `set` wins in each iteration.
#[test]
fn same_seed_picks_the_same_winners() {
    fn run() -> Vec<u32> {
        let winners = Arc::new(Mutex::new(Vec::new()));
        let seen = winners.clone();
        check_random_with_seed(
            move || {
                let cell = Arc::new(SetOnce::new());
                let setters = (1..=3)
                    .map(|value| {
                        let cell = cell.clone();
                        thread::spawn(move || {
                            let _ = cell.set(value);
                        })
                    })
                    .collect::<Vec<_>>();
                for setter in setters {
                    setter.join().unwrap();
                }
                seen.lock().unwrap().push(*cell.get().unwrap());
            },
            0xc0ffee,
            50,
        );
        Arc::try_unwrap(winners).unwrap().into_inner().unwrap()
    }

    let first = run();
    assert_eq!(first.len(), 50);
    assert!(first.iter().collect::<BTreeSet<_>>().len() > 1, "{first:?}");
    assert_eq!(first, run());
}
