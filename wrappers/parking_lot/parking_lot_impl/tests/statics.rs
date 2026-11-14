//! A primitive in a `static` outlives the Shuttle execution that used it, and two Shuttle tests
//! running at the same time on different threads share it. Its Shuttle state belongs to the
//! running execution, so every execution starts with it unlocked, unwaited and uncompleted.
//! The data a lock protects is not part of that state: tests running at the same time take turns
//! on it, and a guard that outlives its execution still holds it, so the next execution that takes
//! the lock reports that guard rather than hand out a second reference to the data
//! (`shuttle_engine::runtime::per_execution::ValueLock`).

use shuttle::scheduler::{DfsScheduler, RandomScheduler};
use shuttle::{Runner, check_dfs, check_random, thread};
use shuttle_parking_lot_impl::{
    Condvar, Mutex, Once, ReentrantMutex, RwLock, const_fair_mutex, const_mutex, const_reentrant_mutex, const_rwlock,
};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The message of the panic that reports a guard that an ended execution left holding a lock's
/// data (see `ValueLock`).
const LEAKED_GUARD: &str = "a guard left behind by an execution that has ended";

/// Runs `run`, which must fail in its second execution, where it takes a lock whose guard the first
/// execution forgot, with the report of that guard.
fn the_second_execution_reports_the_forgotten_guard(run: impl FnOnce()) {
    let panic = catch_unwind(AssertUnwindSafe(run)).expect_err("the second execution reports the forgotten guard");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or_default();
    assert!(message.contains(LEAKED_GUARD), "another panic: {message}");
}

/// An execution that ends with a `static` lock held (a forgotten guard here; a failed execution
/// leaks the guards of its tasks the same way) leaves the next execution an unlocked lock. The
/// forgotten guard still holds the lock's data, though, so the next execution's `lock` reports it,
/// once `is_locked` has shown that the lock itself is free.
#[test]
fn a_static_mutex_starts_every_execution_unlocked() {
    static LOCK: Mutex<()> = const_mutex(());
    the_second_execution_reports_the_forgotten_guard(|| {
        check_random(
            || {
                assert!(!LOCK.is_locked());
                std::mem::forget(LOCK.lock());
                assert!(LOCK.is_locked());
            },
            10,
        )
    });
}

#[test]
fn a_static_fair_mutex_starts_every_execution_unlocked() {
    static LOCK: shuttle_parking_lot_impl::FairMutex<()> = const_fair_mutex(());
    the_second_execution_reports_the_forgotten_guard(|| {
        check_random(
            || {
                assert!(!LOCK.is_locked());
                std::mem::forget(LOCK.lock());
            },
            10,
        )
    });
}

#[test]
fn a_static_rwlock_starts_every_execution_unlocked() {
    static WRITTEN: RwLock<()> = const_rwlock(());
    the_second_execution_reports_the_forgotten_guard(|| {
        check_random(
            || {
                assert!(!WRITTEN.is_locked());
                std::mem::forget(WRITTEN.write());
                assert!(WRITTEN.is_locked_exclusive());
            },
            10,
        )
    });
    // A lock of its own: `WRITTEN`'s forgotten guard would be reported here too.
    static READ: RwLock<()> = const_rwlock(());
    the_second_execution_reports_the_forgotten_guard(|| {
        check_random(
            || {
                assert!(!READ.is_locked());
                std::mem::forget(READ.upgradable_read());
                std::mem::forget(READ.read());
            },
            10,
        )
    });
}

/// `lock_api` keeps a `ReentrantMutex`'s owner inside the lock, so a forgotten guard leaves the
/// owner of the previous execution there. Thread ids are unique across executions, so no task of
/// the next execution mistakes itself for that owner, and a second task still can't get in while
/// the first holds the lock. The forgotten guard still holds the data, so the next execution's
/// `lock` reports it.
///
/// `lock_api` also keeps the lock count inside the lock and `debug_assert`s that it is zero when a
/// task has taken the raw mutex. The raw mutex reports the forgotten guard first, so that assertion
/// is never reached, in a debug build or a release one.
#[test]
fn a_static_reentrant_mutex_starts_every_execution_unowned() {
    static LOCK: ReentrantMutex<()> = const_reentrant_mutex(());
    the_second_execution_reports_the_forgotten_guard(|| {
        check_random(
            || {
                assert!(!LOCK.is_owned_by_current_thread());
                assert!(!LOCK.is_locked());
                let guard = LOCK.lock();
                let other = thread::spawn(|| {
                    assert!(!LOCK.is_owned_by_current_thread());
                    assert!(LOCK.try_lock().is_none(), "two tasks hold a reentrant mutex");
                });
                other.join().unwrap();
                std::mem::forget(guard);
            },
            20,
        )
    });
}

/// The protected data is the caller's, kept inside the lock, so it carries over. A test that needs
/// it to start afresh puts the lock in a `shuttle::lazy_static!`.
#[test]
fn the_data_of_a_static_lock_carries_over() {
    static COUNT: Mutex<usize> = const_mutex(0);
    check_random(|| *COUNT.lock() += 1, 10);
    assert_eq!(*data_between_runs(&COUNT), 10);
}

/// A `static` `Once` runs a closure in every execution.
#[test]
fn a_static_once_runs_once_per_execution() {
    static INIT: Once = Once::new();
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    let executions = check_dfs_count(|| {
        let racer = thread::spawn(|| INIT.call_once(|| _ = RUNS.fetch_add(1, Ordering::SeqCst)));
        INIT.call_once(|| _ = RUNS.fetch_add(1, Ordering::SeqCst));
        racer.join().unwrap();
        assert!(INIT.state().done());
    });
    assert!(executions > 1);
    assert_eq!(RUNS.load(Ordering::SeqCst), executions);
}

/// A failed execution leaves its waiters behind. The next one starts with nobody waiting.
#[test]
fn a_static_condvar_starts_every_execution_without_waiters() {
    static LOCK: Mutex<bool> = const_mutex(false);
    static CONDVAR: Condvar = Condvar::new();
    let failed = catch_unwind(AssertUnwindSafe(|| {
        check_dfs(
            || {
                thread::spawn(|| {
                    let mut ready = LOCK.lock();
                    while !*ready {
                        CONDVAR.wait(&mut ready);
                    }
                });
            },
            None,
        )
    }));
    assert!(failed.is_err(), "the waiter waits forever, which is a deadlock");
    check_dfs(
        || {
            assert!(
                !CONDVAR.notify_one(),
                "a waiter from the failed execution is still waiting"
            );
            assert_eq!(CONDVAR.notify_all(), 0);
        },
        None,
    );
}

/// Several Shuttle tests running at once on different threads, as `cargo test` runs them, use the
/// same `static`s. Each execution has its own state for them: the executions don't block each
/// other's tasks, see each other's locks held, wake each other's tasks or complete each other's
/// `Once`s.
///
/// The data of a lock is the lock's, not the execution's, so two executions that hold one `static`
/// lock at the same time are reported, as a forgotten guard is above. So each thread here takes
/// locks of its own, and looks at the others' while their executions take them. (Two executions
/// holding a `static` `ReentrantMutex` at once would also race on the lock count `lock_api` keeps
/// inside it; see the crate docs.)
#[test]
fn statics_shared_by_tests_running_at_the_same_time() {
    const THREADS: usize = 3;
    static LOCKS: [Mutex<()>; THREADS] = [const_mutex(()), const_mutex(()), const_mutex(())];
    static RWLOCKS: [RwLock<()>; THREADS] = [const_rwlock(()), const_rwlock(()), const_rwlock(())];
    static CONDVAR: Condvar = Condvar::new();
    static INIT: Once = Once::new();

    let runs = Arc::new(AtomicUsize::new(0));
    let mut executions = 0;
    for _ in 0..20 {
        let threads = (0..THREADS)
            .map(|me| {
                let runs = Arc::clone(&runs);
                std::thread::spawn(move || {
                    let runner = Runner::new(DfsScheduler::new(Some(50), false), Default::default());
                    runner.run(move || {
                        let runs = Arc::clone(&runs);
                        // The condvar is shared; the flag it waits for is this execution's.
                        let flag = Arc::new(Mutex::new(false));
                        let other = {
                            let flag = Arc::clone(&flag);
                            thread::spawn(move || {
                                drop(LOCKS[me].lock());
                                drop(RWLOCKS[me].write());
                                INIT.call_once(|| _ = runs.fetch_add(1, Ordering::SeqCst));
                                *flag.lock() = true;
                                CONDVAR.notify_one();
                            })
                        };
                        drop(LOCKS[me].lock());
                        drop(RWLOCKS[me].read());
                        // The other threads' executions take these at the same time; this one
                        // doesn't, so here they are free.
                        for them in (0..THREADS).filter(|&them| them != me) {
                            assert!(!LOCKS[them].is_locked(), "another execution's lock is held here");
                            assert!(!RWLOCKS[them].is_locked(), "another execution's rwlock is held here");
                        }
                        let mut ready = flag.lock();
                        CONDVAR.wait_while(&mut ready, |ready| !*ready);
                        drop(ready);
                        other.join().unwrap();
                        assert!(INIT.state().done());
                    })
                })
            })
            .collect::<Vec<_>>();
        executions += threads.into_iter().map(|t| t.join().unwrap()).sum::<usize>();
    }
    // Every execution ran the `Once`'s closure itself.
    assert_eq!(runs.load(Ordering::SeqCst), executions);
}

/// A task that waits on a condvar gives the mutex's data up with the mutex, so a test that runs at
/// the same time can take the `static` mutex meanwhile, and the lock that ends the wait takes it
/// back once that test has let it go.
#[test]
fn a_condvar_wait_holds_no_data() {
    static GATE: Mutex<usize> = const_mutex(0);
    static WAKE: Condvar = Condvar::new();
    static WAITING: shuttle::sync::atomic::AtomicBool = shuttle::sync::atomic::AtomicBool::new(false);
    let (waits, is_waiting) = std::sync::mpsc::channel();
    let (taken, was_taken) = std::sync::mpsc::channel::<()>();
    let was_taken = std::sync::Mutex::new(was_taken);
    let first = std::thread::spawn(move || {
        check_random(
            move || {
                let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
                let waiter = {
                    let ready = Arc::clone(&ready);
                    thread::spawn(move || {
                        let mut guard = GATE.lock();
                        WAITING.store(true, Ordering::SeqCst);
                        while !ready.load(Ordering::SeqCst) {
                            WAKE.wait(&mut guard);
                        }
                        *guard += 1;
                    })
                };
                while !WAITING.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                // Taken once the waiter's wait has given it up: now nothing in this execution holds it.
                drop(GATE.lock());
                waits.send(()).unwrap();
                was_taken.lock().unwrap().recv().unwrap();
                let guard = GATE.lock();
                ready.store(true, Ordering::SeqCst);
                WAKE.notify_all();
                drop(guard);
                waiter.join().unwrap();
            },
            1,
        )
    });
    is_waiting.recv().unwrap();
    // Another test, while the first one's task waits.
    check_random(|| *GATE.lock() += 1, 1);
    taken.send(()).unwrap();
    first.join().unwrap();
    assert_eq!(*data_between_runs(&GATE), 2);
}

/// The message of the panic for a guard of a test that runs at the same time on the thread
/// `while_another_test_holds` names "holder" (see `ValueLock`).
const HELD_BY_ANOTHER_TEST: &str = "a guard of the execution on thread 'holder', which runs at the same time";

/// The message of a panic.
fn message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

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
            message(&*result.expect_err("a test that takes a lock that another test holds fails"))
        })
        .collect();
    done.send(()).unwrap();
    holder.join().unwrap();
    messages
}

/// A test that takes a `static` lock whose data a test running at the same time holds panics,
/// naming that test's thread, for every kind of lock and of acquisition, `try_*` ones too, which
/// the lock's state grants, and reads of a lock that the other test reads. The reentrant mutex's
/// lock count, which `lock_api` keeps inside the lock, is left to its holder.
#[test]
fn a_static_lock_held_by_a_test_running_at_the_same_time_is_reported() {
    static LOCK: Mutex<()> = const_mutex(());
    static RWLOCK: RwLock<()> = const_rwlock(());
    static REENTRANT: ReentrantMutex<()> = const_reentrant_mutex(());
    static FAIR: shuttle_parking_lot_impl::FairMutex<()> = const_fair_mutex(());
    let messages = while_another_test_holds(
        |meanwhile| {
            let _lock = LOCK.lock();
            let _read = RWLOCK.read();
            let _reentrant = REENTRANT.lock();
            let _again = REENTRANT.lock();
            let _fair = FAIR.lock();
            meanwhile();
        },
        vec![
            || drop(LOCK.lock()),
            || drop(LOCK.try_lock()),
            || drop(RWLOCK.read()),
            || drop(RWLOCK.try_read()),
            || drop(RWLOCK.write()),
            || drop(RWLOCK.upgradable_read()),
            || drop(REENTRANT.lock()),
            || drop(FAIR.lock()),
        ],
    );
    for message in messages {
        assert!(message.contains(HELD_BY_ANOTHER_TEST), "another panic: {message}");
    }
}

/// The lock that ends a condvar wait takes the mutex's data back. A test on another thread that
/// took the `static` mutex while the wait gave it up, and still holds it, is reported there.
#[test]
fn the_lock_that_ends_a_condvar_wait_reports_another_tests_guard() {
    static GATE: Mutex<()> = const_mutex(());
    static WAKE: Condvar = Condvar::new();
    static WAITING: shuttle::sync::atomic::AtomicBool = shuttle::sync::atomic::AtomicBool::new(false);
    let (waits, is_waiting) = std::sync::mpsc::channel();
    let (holds, other_holds) = std::sync::mpsc::channel();
    let other_holds = std::sync::Mutex::new(other_holds);
    let (done, is_done) = std::sync::mpsc::channel::<()>();
    let is_done = std::sync::Mutex::new(is_done);
    let first = std::thread::spawn(move || {
        check_random(
            move || {
                let waiter = thread::spawn(|| {
                    let mut guard = GATE.lock();
                    WAITING.store(true, Ordering::SeqCst);
                    // Woken once, and then takes the mutex back, which the other test holds.
                    WAKE.wait(&mut guard);
                });
                while !WAITING.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                drop(GATE.lock());
                waits.send(()).unwrap();
                other_holds.lock().unwrap().recv().unwrap();
                WAKE.notify_all();
                waiter.join().unwrap();
            },
            1,
        )
    });
    let other = std::thread::Builder::new()
        .name("holder".into())
        .spawn(move || {
            is_waiting.recv().unwrap();
            check_random(
                move || {
                    let _guard = GATE.lock();
                    holds.send(()).unwrap();
                    is_done.lock().unwrap().recv().unwrap();
                },
                1,
            )
        })
        .unwrap();
    let message = message(
        &*first
            .join()
            .expect_err("the waiter's lock reports the other test's guard"),
    );
    assert!(message.contains(HELD_BY_ANOTHER_TEST), "another panic: {message}");
    done.send(()).unwrap();
    other.join().unwrap();
}

/// The number of executions of an exhaustive check of `f`.
fn check_dfs_count(f: impl Fn() + Send + Sync + 'static) -> usize {
    Runner::new(DfsScheduler::new(None, false), Default::default()).run(f)
}

/// A lock's data, between Shuttle runs, where the lock can't be taken (it is a Shuttle primitive).
#[allow(clippy::mut_from_ref)]
fn data_between_runs<T>(lock: &Mutex<T>) -> &mut T {
    // Safety: called only between Shuttle runs, when no task holds or uses the lock.
    unsafe { &mut *lock.data_ptr() }
}

/// The control: the same `static`s on the **real** `parking_lot` carry their state from one
/// execution into the next, which is what this crate's per-execution state fixes. (A real lock that
/// a task of another execution holds would block the OS thread that every task of this one runs on,
/// so the control only looks, and never blocks.)
#[test]
fn real_parking_lot_statics_do_not_reset() {
    static REAL_LOCK: parking_lot::Mutex<()> = parking_lot::const_mutex(());
    static REAL_INIT: parking_lot::Once = parking_lot::Once::new();
    static REAL_RUNS: AtomicUsize = AtomicUsize::new(0);
    static SEEN_LOCKED: AtomicUsize = AtomicUsize::new(0);

    // A random run rather than an exhaustive one: nothing in the body is a scheduling point, so
    // `check_dfs` would explore a single execution.
    const EXECUTIONS: usize = 5;
    let executions = Runner::new(RandomScheduler::new(EXECUTIONS), Default::default()).run(|| {
        if REAL_LOCK.is_locked() {
            SEEN_LOCKED.fetch_add(1, Ordering::SeqCst);
        } else {
            std::mem::forget(REAL_LOCK.lock());
        }
        REAL_INIT.call_once(|| _ = REAL_RUNS.fetch_add(1, Ordering::SeqCst));
    });

    assert_eq!(executions, EXECUTIONS);
    // One closure in the whole process, not one per execution.
    assert_eq!(REAL_RUNS.load(Ordering::SeqCst), 1);
    // Every execution after the first found the lock still held.
    assert_eq!(SEEN_LOCKED.load(Ordering::SeqCst), executions - 1);
}
