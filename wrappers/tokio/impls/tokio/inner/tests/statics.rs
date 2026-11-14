//! Tests that a primitive in a `static`, which outlives an execution, has state of its own in each
//! execution, so that one execution cannot be changed by the ones before it. See
//! `shuttle_engine::runtime::per_execution`.

use shuttle::sync::atomic::{AtomicBool, Ordering};
use shuttle::{check_random, check_uncontrolled_nondeterminism, future, thread};
use shuttle_tokio_impl_inner::sync::{Mutex, Notify, OnceCell, RwLock, Semaphore};
use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};
use test_log::test;

/// How many executions a test has run so far. Each test has its own.
fn execution(counter: &StdAtomicUsize) -> usize {
    counter.fetch_add(1, StdOrdering::SeqCst)
}

#[test]
fn semaphore_resets() {
    static SEMAPHORE: Semaphore = Semaphore::const_new(2);
    check_random(
        || {
            assert_eq!(SEMAPHORE.available_permits(), 2);
            assert!(!SEMAPHORE.is_closed());
            let permit = SEMAPHORE
                .try_acquire_many(2)
                .expect("a static semaphore starts with its permits");
            drop(permit);
            SEMAPHORE.close();
        },
        10,
    );
}

#[test]
fn once_cell_resets() {
    static CELL: OnceCell<usize> = OnceCell::const_new();
    static EXECUTIONS: StdAtomicUsize = StdAtomicUsize::new(0);
    check_random(
        || {
            let n = execution(&EXECUTIONS);
            assert_eq!(CELL.get(), None, "a static OnceCell starts an execution empty");
            assert!(!CELL.initialized());
            CELL.set(n).expect("the cell is empty");
            assert_eq!(CELL.get(), Some(&n));
            assert!(
                CELL.set(n).is_err(),
                "the cell keeps the value for the rest of the execution"
            );
        },
        10,
    );
}

/// A `OnceCell` created with a value is initialized from the start, so no execution can change it
/// and every execution sees the same value.
#[test]
fn once_cell_with_a_value_is_shared() {
    static CELL: OnceCell<usize> = OnceCell::const_new_with(7);
    check_random(
        || {
            assert_eq!(CELL.get(), Some(&7));
            assert!(CELL.set(8).is_err());
        },
        10,
    );
}

#[test]
fn once_cell_init_runs_in_every_execution() {
    static CELL: OnceCell<usize> = OnceCell::const_new();
    static INITIALIZERS: StdAtomicUsize = StdAtomicUsize::new(0);
    static EXECUTIONS: StdAtomicUsize = StdAtomicUsize::new(0);
    check_random(
        || {
            execution(&EXECUTIONS);
            future::block_on(async {
                let value = CELL
                    .get_or_init(|| async {
                        INITIALIZERS.fetch_add(1, StdOrdering::SeqCst);
                        42
                    })
                    .await;
                assert_eq!(*value, 42);
                // The cell is initialized for the rest of the execution, so a second call does not
                // run its initializer.
                assert_eq!(*CELL.get_or_init(|| async { unreachable!() }).await, 42);
            });
        },
        10,
    );
    assert_eq!(
        INITIALIZERS.load(StdOrdering::SeqCst),
        EXECUTIONS.load(StdOrdering::SeqCst),
        "one initializer per execution"
    );
}

#[test]
fn notify_resets() {
    static NOTIFY: Notify = Notify::const_new();
    check_random(
        || {
            // A permit that `notify_one` stored in an earlier execution would make this `notified`
            // complete without a notification of this execution.
            let waiter = future::spawn(async { NOTIFY.notified().await });
            future::block_on(async {
                thread::yield_now();
                NOTIFY.notify_one();
                waiter.await.unwrap();
            });
            // Leave a permit behind for the next execution to not find.
            NOTIFY.notify_one();
        },
        20,
    );
}

/// `check_uncontrolled_nondeterminism` replays every schedule it runs and fails if the second run
/// diverges, which is what a `static` that carried state over causes.
#[test]
fn uncontrolled_nondeterminism_with_statics() {
    static SEMAPHORE: Semaphore = Semaphore::const_new(1);
    static CELL: OnceCell<usize> = OnceCell::const_new();
    static NOTIFY: Notify = Notify::const_new();
    static SPAWNED: AtomicBool = AtomicBool::new(false);

    check_uncontrolled_nondeterminism(
        || {
            future::block_on(async {
                let permit = SEMAPHORE.acquire().await.unwrap();
                CELL.set(1).unwrap();
                drop(permit);

                // The number of tasks this execution spawns depends on the statics, so an
                // execution that saw a leftover value would take a different number of scheduling
                // decisions.
                if !SPAWNED.swap(true, Ordering::SeqCst) {
                    let waiter = future::spawn(async { NOTIFY.notified().await });
                    // `notify_one` leaves a permit if the waiter has not registered yet, so the
                    // waiter completes whichever order they run in.
                    NOTIFY.notify_one();
                    waiter.await.unwrap();
                }
            });
        },
        50,
    );
}

/// The constructors work outside of a Shuttle execution, as tokio's do, so that a `static` can hold
/// one and code that runs before or after a test can create one. Dropping them there works too:
/// the destructors of the channels close semaphores and notify waiters, which used to take a
/// scheduling point unconditionally and panic in a destructor.
#[test]
fn constructors_and_destructors_outside_an_execution() {
    use shuttle_tokio_impl_inner::sync::{mpsc, oneshot, watch};

    drop(Mutex::new(0));
    drop(RwLock::new(0));
    drop(Semaphore::new(1));
    drop(Notify::new());
    drop(OnceCell::<usize>::new());
    drop(OnceCell::new_with(Some(1)));
    drop(mpsc::channel::<usize>(1));
    drop(mpsc::unbounded_channel::<usize>());
    drop(oneshot::channel::<usize>());
    drop(watch::channel(0usize));
}
