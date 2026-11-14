//! Tests that run at the same time on different threads, as `cargo test` runs the tests of a binary,
//! and share a `static` lock. Each execution has the lock's state to itself, but they share its
//! value, so two can't hold it at once: the second panics, saying whose guard holds the value. See
//! `shuttle_engine::runtime::per_execution::ValueLock`.
//!
//! `statics.rs` has the tests of statics that one test uses, one execution after another.

use shuttle::scheduler::RandomScheduler;
use shuttle::sync::atomic::{AtomicBool, Ordering};
use shuttle::sync::{Condvar, Mutex, RwLock};
use shuttle::{check_dfs, check_random, thread, Config, MaxSteps, Runner};
use std::any::Any;
use std::panic::catch_unwind;
use std::sync::{mpsc, Arc, Mutex as StdMutex};
use test_log::test;

/// The message of the panic for a guard of a test that runs at the same time on the thread
/// `while_another_test_holds` names "holder".
const HELD_BY_ANOTHER_TEST: &str = "a guard of the execution on thread 'holder', which runs at the same time";

/// The message of the panic for a guard that an execution which has ended left behind.
const LEFT_BEHIND: &str = "a guard left behind by an execution that has ended";

/// The message of a panic.
fn message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// Runs `hold` as a test of its own, in one execution, on a thread named "holder". `hold` takes some
/// `static` locks and, while it holds them, calls the function it is given, which runs each of
/// `tries` as a test of its own, on another thread, at the same time. Each must fail; returns their
/// messages.
fn while_another_test_holds(hold: impl Fn(&dyn Fn()) + Send + Sync + 'static, tries: Vec<fn()>) -> Vec<String> {
    let (held, holding) = mpsc::channel();
    let (done, is_done) = mpsc::channel::<()>();
    let is_done = StdMutex::new(is_done);
    let holder = std::thread::Builder::new()
        .name("holder".into())
        .spawn(move || {
            check_dfs(
                move || {
                    hold(&|| {
                        held.send(()).unwrap();
                        is_done.lock().unwrap().recv().unwrap();
                    })
                },
                None,
            )
        })
        .unwrap();
    holding.recv().unwrap();
    let messages = tries
        .into_iter()
        .map(|tried| {
            let result = std::thread::spawn(move || check_dfs(tried, None)).join();
            message(&*result.expect_err("a test that takes a lock that another test holds fails"))
        })
        .collect();
    done.send(()).unwrap();
    holder.join().unwrap();
    messages
}

/// A test that locks a `static` mutex that a test running at the same time holds panics, naming
/// that test's thread, also from a `try_lock`, which the mutex's state grants: failing it would
/// make the execution depend on the other test.
#[test]
fn a_static_mutex_held_by_a_test_running_at_the_same_time_is_reported() {
    static LOCK: Mutex<usize> = Mutex::new(0);
    let messages = while_another_test_holds(
        |meanwhile| {
            let _guard = LOCK.lock().unwrap();
            meanwhile();
        },
        vec![|| drop(LOCK.lock()), || drop(LOCK.try_lock())],
    );
    for message in messages {
        assert!(message.contains(HELD_BY_ANOTHER_TEST), "another panic: {message}");
    }
}

/// The same for a `static` rwlock, even when both tests only read it: two executions can't hold
/// the value at once, whatever their guards are.
#[test]
fn a_static_rwlock_held_by_a_test_running_at_the_same_time_is_reported_even_to_a_reader() {
    static LOCK: RwLock<usize> = RwLock::new(0);
    let messages = while_another_test_holds(
        |meanwhile| {
            let _guard = LOCK.read().unwrap();
            meanwhile();
        },
        vec![|| drop(LOCK.read()), || drop(LOCK.try_read()), || drop(LOCK.write())],
    );
    for message in messages {
        assert!(message.contains(HELD_BY_ANOTHER_TEST), "another panic: {message}");
    }
}

/// A task that waits on a condvar gives the mutex's value up with the mutex, so a test that runs at
/// the same time can take the `static` mutex meanwhile, and the lock that ends the wait takes it
/// back once that test has let it go.
#[test]
fn a_condvar_wait_holds_no_value() {
    static LOCK: Mutex<usize> = Mutex::new(0);
    static READY: Condvar = Condvar::new();
    static WAITING: AtomicBool = AtomicBool::new(false);
    let (waits, is_waiting) = mpsc::channel();
    let (taken, was_taken) = mpsc::channel::<()>();
    let was_taken = StdMutex::new(was_taken);
    let first = std::thread::spawn(move || {
        check_random(
            move || {
                let ready = Arc::new(AtomicBool::new(false));
                let waiter = {
                    let ready = Arc::clone(&ready);
                    thread::spawn(move || {
                        let mut guard = LOCK.lock().unwrap();
                        WAITING.store(true, Ordering::SeqCst);
                        while !ready.load(Ordering::SeqCst) {
                            guard = READY.wait(guard).unwrap();
                        }
                        *guard += 1;
                    })
                };
                while !WAITING.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                // Taken once the waiter's wait has given it up: now nothing in this execution holds it.
                drop(LOCK.lock().unwrap());
                waits.send(()).unwrap();
                was_taken.lock().unwrap().recv().unwrap();
                let guard = LOCK.lock().unwrap();
                ready.store(true, Ordering::SeqCst);
                READY.notify_all();
                drop(guard);
                waiter.join().unwrap();
            },
            1,
        )
    });
    is_waiting.recv().unwrap();
    // Another test, while the first one's task waits.
    check_dfs(|| *LOCK.lock().unwrap() += 1, None);
    taken.send(()).unwrap();
    first.join().unwrap();
    check_dfs(|| assert_eq!(*LOCK.lock().unwrap(), 2), None);
}

/// Runs a test whose one execution stops at its step bound while it holds `lock`, so that the
/// stack with the guard on it is leaked.
fn run_out_of_steps_holding(lock: &'static Mutex<usize>) {
    let mut config = Config::new();
    config.max_steps = MaxSteps::ContinueAfter(20);
    let executions = Runner::new(RandomScheduler::new_from_seed(0x5EED, 1), config).run(move || {
        let _guard = lock.lock().unwrap();
        loop {
            thread::yield_now();
        }
    });
    assert_eq!(executions, 1);
}

/// A guard that an execution of one test left behind holds the value for good, which a test on
/// another thread that then takes the lock reports, as left behind rather than as held by a test
/// running at the same time.
#[test]
fn a_guard_left_behind_by_another_test_is_reported() {
    static LOCK: Mutex<usize> = Mutex::new(0);
    std::thread::spawn(|| run_out_of_steps_holding(&LOCK)).join().unwrap();
    let result = std::thread::spawn(|| check_dfs(|| drop(LOCK.lock()), None)).join();
    let message = message(&*result.expect_err("the guard left behind is reported"));
    assert!(message.contains(LEFT_BEHIND), "another panic: {message}");
    assert!(!message.contains("runs at the same time"), "{message}");
}

/// Poisoning is part of a lock's state, which is each execution's own. A test whose execution
/// poisons a `static` mutex and rwlock still sees them poisoned after a test on another thread has
/// used them, and that test never sees them poisoned. The executions run at the same time, though
/// they don't hold the locks at once.
#[test]
fn poisoning_stays_in_its_execution() {
    static MUTEX: Mutex<()> = Mutex::new(());
    static RWLOCK: RwLock<()> = RwLock::new(());
    let (poisoned, has_poisoned) = mpsc::channel();
    let (used, was_used) = mpsc::channel::<()>();
    let was_used = StdMutex::new(was_used);
    let poisoner = std::thread::spawn(move || {
        check_random(
            move || {
                let result = catch_unwind(|| {
                    let _mutex = MUTEX.lock();
                    let _rwlock = RWLOCK.write();
                    panic!("poison both");
                });
                assert!(result.is_err());
                assert!(MUTEX.is_poisoned() && RWLOCK.is_poisoned());
                poisoned.send(()).unwrap();
                was_used.lock().unwrap().recv().unwrap();
                assert!(MUTEX.is_poisoned(), "another execution cleared this one's poisoning");
                assert!(MUTEX.lock().is_err(), "another execution cleared this one's poisoning");
                assert!(RWLOCK.is_poisoned(), "another execution cleared this one's poisoning");
                assert!(RWLOCK.read().is_err(), "another execution cleared this one's poisoning");
            },
            1,
        )
    });
    has_poisoned.recv().unwrap();
    check_dfs(
        || {
            assert!(!MUTEX.is_poisoned(), "another execution's poisoning is here");
            assert!(MUTEX.lock().is_ok(), "another execution's poisoning is here");
            assert!(!RWLOCK.is_poisoned(), "another execution's poisoning is here");
            assert!(RWLOCK.write().is_ok(), "another execution's poisoning is here");
        },
        None,
    );
    used.send(()).unwrap();
    poisoner.join().unwrap();
}
