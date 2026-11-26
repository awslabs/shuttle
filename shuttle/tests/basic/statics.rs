//! Tests that a primitive in a `static`, which outlives an execution, has state of its own in each
//! execution, so that one execution cannot be changed by the ones before it. See
//! `shuttle_engine::runtime::per_execution`.
//!
//! `once.rs`, `once_lock.rs` and `lazy_static.rs` have the same tests for the types whose state was
//! already per execution before.

use shuttle::current::context_switches;
use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use shuttle::scheduler::RandomScheduler;
use shuttle::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use shuttle::sync::{mpsc, Barrier, Condvar, Mutex, Once, RwLock};
use shuttle::{check_dfs, check_random, check_uncontrolled_nondeterminism, current, thread, Config, MaxSteps, Runner};
use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};
use test_log::test;

/// How many executions a test has run so far. Each test has its own.
fn execution(counter: &StdAtomicUsize) -> usize {
    counter.fetch_add(1, StdOrdering::SeqCst)
}

#[test]
fn atomic_resets() {
    static VALUE: AtomicUsize = AtomicUsize::new(5);
    static FLAG: AtomicBool = AtomicBool::new(true);
    check_random(
        || {
            assert_eq!(VALUE.load(Ordering::SeqCst), 5);
            assert!(FLAG.load(Ordering::SeqCst));
            VALUE.fetch_add(1, Ordering::SeqCst);
            FLAG.store(false, Ordering::SeqCst);
            assert_eq!(VALUE.swap(0, Ordering::SeqCst), 6);
        },
        10,
    );
}

#[test]
fn atomic_ptr_resets() {
    static TARGET: StdAtomicUsize = StdAtomicUsize::new(7);
    static POINTER: AtomicPtr<StdAtomicUsize> = AtomicPtr::new(std::ptr::null_mut());
    check_random(
        || {
            assert!(POINTER.load(Ordering::SeqCst).is_null());
            POINTER.store(&TARGET as *const _ as *mut _, Ordering::SeqCst);
            // Safety: the pointer is to a `static`, and is only read through.
            let value = unsafe { &*POINTER.load(Ordering::SeqCst) };
            assert_eq!(value.load(StdOrdering::SeqCst), 7);
        },
        10,
    );
}

#[test]
fn mutex_resets() {
    static LOCK: Mutex<usize> = Mutex::new(0);
    static EXECUTIONS: StdAtomicUsize = StdAtomicUsize::new(0);
    check_random(
        || {
            let executions_before = execution(&EXECUTIONS);

            // Not held, and not poisoned by the execution that panicked below.
            let mut guard = LOCK.try_lock().expect("a static mutex starts an execution unlocked");
            // The value, unlike the state of the lock, belongs to the mutex rather than to an
            // execution, so every execution adds to the same one.
            assert_eq!(*guard, executions_before);
            *guard += 1;
            drop(guard);

            let result = std::panic::catch_unwind(|| {
                let _guard = LOCK.lock().expect("a static mutex starts an execution unpoisoned");
                panic!("poison the mutex");
            });
            assert!(result.is_err());
            assert!(LOCK.lock().is_err(), "the poisoning is visible in its own execution");
        },
        10,
    );
}

#[test]
fn rwlock_resets() {
    static LOCK: RwLock<usize> = RwLock::new(0);
    static EXECUTIONS: StdAtomicUsize = StdAtomicUsize::new(0);
    check_random(
        || {
            let executions_before = execution(&EXECUTIONS);

            let mut guard = LOCK.try_write().expect("a static rwlock starts an execution unlocked");
            // As for `Mutex`, the value belongs to the lock rather than to an execution.
            assert_eq!(*guard, executions_before);
            *guard += 1;
            drop(guard);

            let result = std::panic::catch_unwind(|| {
                let _guard = LOCK.write().expect("a static rwlock starts an execution unpoisoned");
                panic!("poison the rwlock");
            });
            assert!(result.is_err());
            assert!(LOCK.read().is_err(), "the poisoning is visible in its own execution");
        },
        10,
    );
}

/// `is_poisoned` reads the poisoning of the current execution, even as the first thing the execution
/// does with the lock, not what an earlier execution left in it.
#[test]
fn poisoning_resets_before_is_poisoned() {
    static MUTEX: Mutex<()> = Mutex::new(());
    static RWLOCK: RwLock<()> = RwLock::new(());
    check_random(
        || {
            assert!(!MUTEX.is_poisoned(), "a static mutex starts an execution unpoisoned");
            assert!(!RWLOCK.is_poisoned(), "a static rwlock starts an execution unpoisoned");
            let result = std::panic::catch_unwind(|| {
                let _mutex = MUTEX.lock().unwrap();
                let _rwlock = RWLOCK.write().unwrap();
                panic!("poison both");
            });
            assert!(result.is_err());
            assert!(MUTEX.is_poisoned());
            assert!(RWLOCK.is_poisoned());
        },
        10,
    );
}

#[test]
fn semaphore_resets() {
    static SEMAPHORE: BatchSemaphore = BatchSemaphore::const_new(2, Fairness::StrictlyFair);
    check_random(
        || {
            assert_eq!(SEMAPHORE.available_permits(), 2);
            assert!(!SEMAPHORE.is_closed());
            SEMAPHORE
                .try_acquire(2)
                .expect("a static semaphore starts with its permits");
            SEMAPHORE.close();
        },
        10,
    );
}

#[test]
fn barrier_resets() {
    static BARRIER: Barrier = Barrier::new(2);
    check_random(
        || {
            // An arrival of an earlier execution would make this one the second, and release it.
            let thread = thread::spawn(|| BARRIER.wait().is_leader());
            let leader_here = BARRIER.wait().is_leader();
            let leader_there = thread.join().unwrap();
            assert_ne!(leader_here, leader_there, "exactly one of the two leads the generation");
        },
        20,
    );
}

/// A `Condvar` whose execution ended while a thread waited on it: the next execution must not find
/// that waiter. An execution that stops at its step bound leaks the stacks of its threads, so the
/// waiter is still there to be found.
#[test]
fn condvar_waiter_does_not_outlive_its_execution() {
    static LOCK: Mutex<bool> = Mutex::new(false);
    static CONDVAR: Condvar = Condvar::new();
    static EXECUTIONS: StdAtomicUsize = StdAtomicUsize::new(0);

    let mut config = Config::new();
    config.max_steps = MaxSteps::ContinueAfter(50);
    let runner = Runner::new(RandomScheduler::new_from_seed(0x5EED, 4), config);
    runner.run(|| {
        if execution(&EXECUTIONS) == 0 {
            // Leave a waiter behind: this execution runs out of steps while the thread waits.
            thread::spawn(|| {
                let guard = LOCK.lock().unwrap();
                let _guard = CONDVAR.wait(guard).unwrap();
            });
            loop {
                thread::yield_now();
            }
        } else {
            // Nothing waits in this execution, so this is a no-op rather than a wake-up of a
            // thread of an execution that has ended.
            CONDVAR.notify_all();
            CONDVAR.notify_one();
        }
    });
}

/// The clock of a `static` primitive does not outlive its execution either, so an execution cannot
/// take causality from the ones before it.
#[test]
fn clock_does_not_outlive_its_execution() {
    static LOCK: Mutex<()> = Mutex::new(());
    static SEMAPHORE: BatchSemaphore = BatchSemaphore::const_new(1, Fairness::StrictlyFair);
    static ATOMIC: AtomicUsize = AtomicUsize::new(0);
    static EXECUTIONS: StdAtomicUsize = StdAtomicUsize::new(0);
    check_random(
        || {
            let first = execution(&EXECUTIONS) == 0;
            for _ in 0..(if first { 20 } else { 1 }) {
                drop(LOCK.lock().unwrap());
                SEMAPHORE.try_acquire(1).unwrap();
                SEMAPHORE.release(1);
                ATOMIC.fetch_add(1, Ordering::SeqCst);
            }
            if !first {
                // Joined with the first execution's operations, the main thread's entry of the
                // clock would be far above the handful of operations of this execution.
                let clock = current::clock();
                assert!(
                    clock.get(0) < 20,
                    "an earlier execution's clock is in this one: {clock:?}"
                );
            }
        },
        3,
    );
}

/// A lock keeps its state per execution, but its value belongs to the lock, so a guard that
/// outlives its execution still holds the value. The next execution is granted the lock by its own
/// state, and reports that rather than hand out a second reference to the value, or wait for a
/// release that can never come. `shared_statics.rs` has the same for tests running at the same time.
#[test]
#[should_panic(
    expected = "a guard from outside of the current execution holds its value: a guard left behind by an \
                execution that has ended"
)]
fn guard_outliving_its_execution_is_reported() {
    static LOCK: Mutex<usize> = Mutex::new(0);
    static EXECUTIONS: StdAtomicUsize = StdAtomicUsize::new(0);

    let mut config = Config::new();
    config.max_steps = MaxSteps::ContinueAfter(50);
    let runner = Runner::new(RandomScheduler::new_from_seed(0x5EED, 2), config);
    runner.run(|| {
        if execution(&EXECUTIONS) == 0 {
            // This execution runs out of steps holding the lock, and its stack is leaked, so the
            // guard lives on.
            let _guard = LOCK.lock().unwrap();
            loop {
                thread::yield_now();
            }
        } else {
            drop(LOCK.lock().unwrap());
        }
    });
}

/// `check_uncontrolled_nondeterminism` replays every schedule it runs and fails if the second run
/// diverges, which is what a `static` that carried state over causes.
#[test]
fn uncontrolled_nondeterminism_with_statics() {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    static SPAWNED: AtomicBool = AtomicBool::new(false);
    static LOCK: Mutex<usize> = Mutex::new(0);
    static RW: RwLock<usize> = RwLock::new(0);
    static BARRIER: Barrier = Barrier::new(2);
    static ONCE: Once = Once::new();
    static SEMAPHORE: BatchSemaphore = BatchSemaphore::const_new(1, Fairness::StrictlyFair);

    check_uncontrolled_nondeterminism(
        || {
            ONCE.call_once(|| {
                COUNT.fetch_add(1, Ordering::SeqCst);
            });
            *LOCK.lock().unwrap() += 1;
            *RW.write().unwrap() += 1;
            SEMAPHORE.try_acquire(1).unwrap();
            SEMAPHORE.release(1);

            // The number of threads this execution spawns depends on the statics, so an execution
            // that saw a leftover value would take a different number of scheduling decisions.
            if !SPAWNED.swap(true, Ordering::SeqCst) {
                let thread = thread::spawn(|| BARRIER.wait());
                BARRIER.wait();
                thread.join().unwrap();
            }
        },
        50,
    );
}

/// Primitives can be created outside of a Shuttle execution, as the ones they model can, so that a
/// `static` can hold one and code that runs before or after a test can touch it.
#[test]
fn constructors_outside_an_execution() {
    let _mutex = Mutex::new(0);
    let _rwlock = RwLock::new(0);
    let _condvar = Condvar::new();
    let _barrier = Barrier::new(2);
    let _once = Once::new();
    let _atomic = AtomicUsize::new(0);
    let _semaphore = BatchSemaphore::new(1, Fairness::StrictlyFair);
    let _channel = mpsc::channel::<usize>();
    let _sync_channel = mpsc::sync_channel::<usize>(1);
}

/// The primitives of an execution behave as they did before their state became per execution: the
/// operations take the same scheduling points, and the state follows the operations.
#[test]
fn instances_of_an_execution_are_unchanged() {
    check_dfs(
        || {
            let lock = Mutex::new(0);
            let rwlock = RwLock::new(0);
            let semaphore = BatchSemaphore::new(1, Fairness::StrictlyFair);
            let atomic = AtomicUsize::new(0);

            let steps = context_switches();
            *lock.lock().unwrap() += 1;
            assert_eq!(context_switches() - steps, 2, "the lock and the release of a mutex");

            let steps = context_switches();
            *rwlock.write().unwrap() += 1;
            assert_eq!(context_switches() - steps, 2, "the lock and the release of an rwlock");

            let steps = context_switches();
            atomic.store(1, Ordering::SeqCst);
            assert_eq!(context_switches() - steps, 1, "a store");

            let steps = context_switches();
            semaphore.try_acquire(1).unwrap();
            semaphore.release(1);
            assert_eq!(context_switches() - steps, 2, "an acquire and a release");

            assert_eq!(*lock.lock().unwrap(), 1);
            assert_eq!(*rwlock.read().unwrap(), 1);
            assert_eq!(atomic.load(Ordering::SeqCst), 1);
            assert_eq!(semaphore.available_permits(), 1);
        },
        Some(1),
    );
}

/// An arrival at a barrier that waits takes a scheduling point too (see
/// `basic::barrier::every_arrival_is_a_separate_step`), which is the one count this change adds.
#[test]
fn barrier_arrival_scheduling_points() {
    check_dfs(
        || {
            let barrier = std::sync::Arc::new(Barrier::new(2));
            let other = {
                let barrier = barrier.clone();
                thread::spawn(move || {
                    let steps = context_switches();
                    barrier.wait();
                    // The arrival, and the step the thread is woken in.
                    assert!(context_switches() - steps >= 1);
                })
            };
            let steps = context_switches();
            barrier.wait();
            assert!(
                context_switches() - steps >= 1,
                "the arrival that releases the generation"
            );
            other.join().unwrap();
        },
        Some(20),
    );
}
