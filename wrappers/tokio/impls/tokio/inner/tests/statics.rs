//! Tests that a primitive in a `static`, which outlives an execution, has state of its own in each
//! execution, so that one execution cannot be changed by the ones before it. See
//! `shuttle_engine::runtime::per_execution`.

use shuttle::sync::atomic::{AtomicBool, Ordering};
use shuttle::{check_random, check_uncontrolled_nondeterminism, future, thread};
use shuttle_tokio_impl_inner::sync::{Mutex, Notify, OnceCell, RwLock, Semaphore, SetOnce};
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

/// A `static` tokio mutex and rwlock are unlocked at the start of every execution, and their value
/// is the one they were created with, which every execution shares.
#[test]
fn locks_reset() {
    static LOCK: Mutex<usize> = Mutex::const_new(0);
    static RW: RwLock<usize> = RwLock::const_new(0);
    static EXECUTIONS: StdAtomicUsize = StdAtomicUsize::new(0);
    check_random(
        || {
            let executions_before = execution(&EXECUTIONS);
            let mut guard = LOCK.try_lock().expect("a static mutex starts an execution unlocked");
            assert_eq!(*guard, executions_before);
            *guard += 1;
            drop(guard);

            let mut guard = RW.try_write().expect("a static rwlock starts an execution unlocked");
            assert_eq!(*guard, executions_before);
            *guard += 1;
            drop(guard);

            // The guards of this execution are gone, so the locks are free again.
            future::block_on(async {
                drop(LOCK.lock().await);
                drop(RW.read().await);
            });
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
fn set_once_resets() {
    static CELL: SetOnce<usize> = SetOnce::const_new();
    static EXECUTIONS: StdAtomicUsize = StdAtomicUsize::new(0);
    check_random(
        || {
            let n = execution(&EXECUTIONS);
            assert_eq!(CELL.get(), None, "a static SetOnce starts an execution empty");
            assert!(!CELL.initialized());
            CELL.set(n).expect("the cell is empty");
            assert_eq!(CELL.get(), Some(&n));
            assert!(CELL.set(n).is_err());
        },
        10,
    );
}

#[test]
fn set_once_with_a_value_is_shared() {
    static CELL: SetOnce<usize> = SetOnce::const_new_with(7);
    check_random(
        || {
            assert_eq!(CELL.get(), Some(&7));
            assert!(CELL.set(8).is_err());
            // A `wait` on a cell that already has a value returns it at once.
            future::block_on(async { assert_eq!(*CELL.wait().await, 7) });
        },
        10,
    );
}

/// A `SetOnce` in a `static` wakes the waiters of its own execution, and no others. A waiter of an
/// execution that ended is gone with the execution.
#[test]
fn set_once_wakes_the_waiters_of_its_execution() {
    static CELL: SetOnce<usize> = SetOnce::const_new();
    check_random(
        || {
            let waiter = future::spawn(async { *CELL.wait().await });
            let setter = future::spawn(async { CELL.set(3) });
            future::block_on(async {
                setter.await.unwrap().unwrap();
                assert_eq!(waiter.await.unwrap(), 3);
            });
        },
        20,
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
    static ONCE: SetOnce<usize> = SetOnce::const_new();
    static NOTIFY: Notify = Notify::const_new();
    static SPAWNED: AtomicBool = AtomicBool::new(false);

    check_uncontrolled_nondeterminism(
        || {
            future::block_on(async {
                let permit = SEMAPHORE.acquire().await.unwrap();
                CELL.set(1).unwrap();
                ONCE.set(1).unwrap();
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
    use shuttle_tokio_impl_inner::sync::{mpsc, oneshot, watch, Barrier};

    drop(Mutex::new(0));
    drop(RwLock::new(0));
    drop(Semaphore::new(1));
    drop(Notify::new());
    drop(OnceCell::<usize>::new());
    drop(OnceCell::new_with(Some(1)));
    drop(SetOnce::<usize>::new());
    drop(SetOnce::from(1));
    drop(Barrier::new(2));
    drop(mpsc::channel::<usize>(1));
    drop(mpsc::unbounded_channel::<usize>());
    drop(oneshot::channel::<usize>());
    drop(watch::channel(0usize));
}

/// The message of the panic for a guard of a test that runs at the same time on the thread
/// `while_another_test_holds` names "holder" (see `ValueLock`).
const HELD_BY_ANOTHER_TEST: &str = "a guard of the execution on thread 'holder', which runs at the same time";

/// Runs `hold` as a test of its own, in one execution, on a thread named "holder". `hold` takes some
/// `static` locks and, while it holds them, calls the function it is given, which runs each of
/// `tries` as a test of its own, on another thread, at the same time. Each must fail; returns their
/// messages.
fn while_another_test_holds(hold: impl Fn(&dyn Fn()) + Send + Sync + 'static, tries: Vec<fn()>) -> Vec<String> {
    let (held, holding) = std::sync::mpsc::channel();
    let (done, is_done) = std::sync::mpsc::channel::<()>();
    let is_done = std::sync::Mutex::new(is_done);
    let holder = std::thread::Builder::new()
        .name("holder".into())
        .spawn(move || {
            check_random(
                move || {
                    hold(&|| {
                        held.send(()).unwrap();
                        is_done.lock().unwrap().recv().unwrap();
                    })
                },
                1,
            )
        })
        .unwrap();
    holding.recv().unwrap();
    let messages = tries
        .into_iter()
        .map(|tried| {
            let result = std::thread::spawn(move || check_random(tried, 1)).join();
            let panic = result.expect_err("a test that takes a lock that another test holds fails");
            panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default()
        })
        .collect();
    done.send(()).unwrap();
    holder.join().unwrap();
    messages
}

/// A test that takes a `static` tokio mutex that a test running at the same time holds panics,
/// naming that test's thread, also from a `try_lock`, which the mutex's state grants: failing it
/// would make the execution depend on the other test.
#[test]
fn a_static_mutex_held_by_a_test_running_at_the_same_time_is_reported() {
    static LOCK: Mutex<usize> = Mutex::const_new(0);
    let messages = while_another_test_holds(
        |meanwhile| {
            future::block_on(async {
                let _guard = LOCK.lock().await;
                meanwhile();
            })
        },
        vec![|| drop(LOCK.try_lock()), || drop(future::block_on(LOCK.lock()))],
    );
    for message in messages {
        assert!(message.contains(HELD_BY_ANOTHER_TEST), "another panic: {message}");
    }
}

/// The same for a `static` tokio rwlock, even when both tests only read it.
#[test]
fn a_static_rwlock_held_by_a_test_running_at_the_same_time_is_reported_even_to_a_reader() {
    static PAIR: RwLock<(usize, usize)> = RwLock::const_new((0, 0));
    let messages = while_another_test_holds(
        |meanwhile| {
            future::block_on(async {
                let _guard = PAIR.read().await;
                meanwhile();
            })
        },
        vec![|| drop(PAIR.try_read()), || drop(future::block_on(PAIR.read())), || {
            drop(future::block_on(PAIR.write()))
        }],
    );
    for message in messages {
        assert!(message.contains(HELD_BY_ANOTHER_TEST), "another panic: {message}");
    }
}
