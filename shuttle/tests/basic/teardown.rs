//! Tests for execution teardown: dropping the tasks that have not finished when an execution ends.
//!
//! Each test parks a task that owns something with a destructor, lets the execution finish, and
//! checks what that destructor can do while teardown drops the task.

use shuttle::current::{self, TaskId};
use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use shuttle::future::{self, block_on, yield_now};
use shuttle::scheduler::RandomScheduler;
use shuttle::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use shuttle::sync::{mpsc, Condvar, Mutex, RwLock};
use shuttle::{check_random, thread, Config, ContinuationFunctionBehavior, Runner};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};
use std::sync::Arc;
use test_log::test;

const ITERATIONS: usize = 10;

/// Runs a closure when dropped.
struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        (self.0.take().unwrap())();
    }
}

/// Spawns and detaches a task that owns `on_drop` and then waits forever, so that the execution
/// ends with the task parked between polls. Returns once the task has started.
fn park_until_teardown(on_drop: impl FnOnce() + 'static) {
    let (started_tx, started_rx) = mpsc::channel();
    drop(future::spawn_local(async move {
        let _on_drop = OnDrop(Some(on_drop));
        started_tx.send(()).unwrap();
        futures::future::pending::<()>().await;
    }));
    started_rx.recv().unwrap();
}

/// Like `park_until_teardown`, but the task is runnable and inside `poll` when the execution ends:
/// it spins on an atomic, and teardown has to unwind its stack to drop it.
fn spin_until_teardown(on_drop: impl FnOnce() + 'static) {
    let (started_tx, started_rx) = mpsc::channel();
    drop(future::spawn_local(async move {
        let _on_drop = OnDrop(Some(on_drop));
        started_tx.send(()).unwrap();
        let spins = AtomicUsize::new(0);
        loop {
            spins.fetch_add(1, Ordering::SeqCst);
        }
    }));
    started_rx.recv().unwrap();
}

/// Checks that each of `num_drops` destructors that `test` sets up ran to completion in every
/// execution, by giving `test` a counter to increment.
fn check_destructors_complete(num_drops: usize, test: impl Fn(Arc<StdAtomicUsize>) + Send + Sync + 'static) {
    let completed = Arc::new(StdAtomicUsize::new(0));
    let counter = completed.clone();
    check_random(move || test(counter.clone()), ITERATIONS);
    assert_eq!(completed.load(StdOrdering::SeqCst), num_drops * ITERATIONS);
}

fn count(counter: &Arc<StdAtomicUsize>) {
    counter.fetch_add(1, StdOrdering::SeqCst);
}

/// The destructors use locks and semaphores.
fn use_locks(counter: Arc<StdAtomicUsize>, park: fn(Box<dyn FnOnce()>)) {
    let mutex = Arc::new(Mutex::new(0));
    let (m, c) = (mutex.clone(), counter.clone());
    park(Box::new(move || {
        *m.lock().unwrap() += 1;
        drop(m.try_lock().unwrap());
        count(&c);
    }));
    let rwlock = Arc::new(RwLock::new(0));
    let c = counter.clone();
    park(Box::new(move || {
        *rwlock.write().unwrap() += 1;
        assert_eq!(*rwlock.read().unwrap(), 1);
        count(&c);
    }));
    let semaphore = BatchSemaphore::new(1, Fairness::StrictlyFair);
    park(Box::new(move || {
        semaphore.try_acquire(1).unwrap();
        assert!(semaphore.try_acquire(1).is_err());
        semaphore.release(1);
        count(&counter);
    }));
}

#[test]
fn destructors_use_locks_during_teardown() {
    check_destructors_complete(3, |counter| use_locks(counter, park_until_teardown));
}

#[test]
fn destructors_use_locks_while_unwinding_a_task_during_teardown() {
    check_destructors_complete(3, |counter| use_locks(counter, spin_until_teardown));
}

/// The destructors use atomics, channels and condition variables.
fn use_atomics_and_channels(counter: Arc<StdAtomicUsize>, park: fn(Box<dyn FnOnce()>)) {
    let flag = Arc::new(AtomicBool::new(false));
    let c = counter.clone();
    park(Box::new(move || {
        flag.store(true, Ordering::SeqCst);
        assert!(flag.swap(false, Ordering::SeqCst));
        count(&c);
    }));
    let (tx, rx) = mpsc::channel();
    let c = counter.clone();
    park(Box::new(move || {
        tx.send(1).unwrap();
        assert_eq!(rx.try_recv(), Ok(1));
        count(&c);
    }));
    let condvar = Arc::new(Condvar::new());
    park(Box::new(move || {
        condvar.notify_one();
        condvar.notify_all();
        count(&counter);
    }));
}

#[test]
fn destructors_use_atomics_and_channels_during_teardown() {
    check_destructors_complete(3, |counter| use_atomics_and_channels(counter, park_until_teardown));
}

#[test]
fn destructors_use_atomics_and_channels_while_unwinding_a_task_during_teardown() {
    check_destructors_complete(3, |counter| use_atomics_and_channels(counter, spin_until_teardown));
}

/// The destructors use tasks: they spawn and abort tasks, yield, and look at the current task.
fn use_tasks(counter: Arc<StdAtomicUsize>, park: fn(Box<dyn FnOnce()>)) {
    let abort_handle = future::spawn(futures::future::pending::<()>()).abort_handle();
    let c = counter.clone();
    park(Box::new(move || {
        assert!(!abort_handle.is_finished());
        abort_handle.abort();
        let _ = current::me();
        let _ = thread::current().id();
        thread::yield_now();
        assert_eq!(block_on(async { 1 }), 1);
        count(&c);
    }));
    park(Box::new(move || {
        drop(future::spawn(async {}));
        drop(thread::spawn(|| {}));
        count(&counter);
    }));
}

#[test]
fn destructors_use_tasks_during_teardown() {
    check_destructors_complete(2, |counter| use_tasks(counter, park_until_teardown));
}

#[test]
fn destructors_use_tasks_while_unwinding_a_task_during_teardown() {
    check_destructors_complete(2, |counter| use_tasks(counter, spin_until_teardown));
}

/// A future that a destructor waits for can wake its task: here, by yielding.
#[test]
fn destructor_waits_for_self_waking_future_during_teardown() {
    check_destructors_complete(1, |counter| {
        park_until_teardown(move || {
            block_on(async {
                yield_now().await;
                yield_now().await;
            });
            count(&counter);
        })
    });
}

/// A task that a destructor spawns during teardown never runs, but is torn down too.
#[test]
fn task_spawned_during_teardown_is_torn_down() {
    check_destructors_complete(1, |counter| {
        park_until_teardown(move || {
            let on_drop = OnDrop(Some(move || count(&counter)));
            drop(future::spawn_local(async move {
                let _on_drop = on_drop;
                unreachable!("tasks do not run during teardown");
            }));
        })
    });
}

/// While teardown drops a task, that task is the current task.
#[test]
fn destructors_run_as_their_task_during_teardown() {
    let ids = Arc::new(std::sync::Mutex::new(Vec::<(TaskId, TaskId)>::new()));
    let ids_clone = ids.clone();
    check_random(
        move || {
            for _ in 0..2 {
                let (id_tx, id_rx) = mpsc::channel();
                let ids = ids_clone.clone();
                drop(future::spawn_local(async move {
                    let me = current::me();
                    let _on_drop = OnDrop(Some(move || ids.lock().unwrap().push((me, current::me()))));
                    id_tx.send(()).unwrap();
                    futures::future::pending::<()>().await;
                }));
                id_rx.recv().unwrap();
            }
        },
        ITERATIONS,
    );
    let ids = ids.lock().unwrap();
    assert_eq!(ids.len(), 2 * ITERATIONS);
    for (spawned_as, dropped_as) in ids.iter() {
        assert_eq!(spawned_as, dropped_as);
    }
}

/// A lock that a task holds when the execution ends is released when teardown drops the task, so
/// the destructors of the tasks that teardown drops after it can take it.
#[test]
fn lock_held_by_task_is_released_during_teardown() {
    check_destructors_complete(1, |counter| {
        let mutex = Arc::new(Mutex::new(0));
        let (locked_tx, locked_rx) = mpsc::channel();
        let m = mutex.clone();
        drop(future::spawn_local(async move {
            let _guard = m.lock().unwrap();
            locked_tx.send(()).unwrap();
            futures::future::pending::<()>().await;
        }));
        locked_rx.recv().unwrap();
        park_until_teardown(move || {
            *mutex.lock().unwrap() += 1;
            count(&counter);
        });
    });
}

/// Thread-local values of the tasks that teardown drops are dropped with them, as their task.
#[test]
fn task_locals_are_dropped_during_teardown() {
    struct Local(Arc<StdAtomicUsize>, TaskId);

    impl Drop for Local {
        fn drop(&mut self) {
            assert_eq!(current::me(), self.1);
            let mutex = Mutex::new(());
            drop(mutex.lock().unwrap());
            count(&self.0);
        }
    }

    shuttle::thread_local! {
        static LOCAL: std::cell::RefCell<Option<Local>> = const { std::cell::RefCell::new(None) };
    }

    check_destructors_complete(1, |counter| {
        let (started_tx, started_rx) = mpsc::channel();
        drop(future::spawn_local(async move {
            LOCAL.with(|local| *local.borrow_mut() = Some(Local(counter, current::me())));
            started_tx.send(()).unwrap();
            futures::future::pending::<()>().await;
        }));
        started_rx.recv().unwrap();
    });
}

/// A panic in a destructor during teardown fails the test like any other panic (rather than
/// aborting the process).
#[test]
#[should_panic(expected = "destructor failed")]
fn panic_in_destructor_during_teardown_fails_test() {
    check_random(|| park_until_teardown(|| panic!("destructor failed")), ITERATIONS);
}

/// A destructor that blocks during teardown can never be woken, which fails the test.
#[test]
#[should_panic(expected = "blocked while it was being dropped at the end of the execution")]
fn blocking_in_destructor_during_teardown_fails_test() {
    check_random(
        || {
            let (tx, rx) = mpsc::channel::<()>();
            park_until_teardown(move || {
                let _tx = tx;
                let _ = rx.recv();
            })
        },
        ITERATIONS,
    );
}

/// Calls `count` when dropped, after using Shuttle primitives.
struct UsesShuttleOnDrop(Arc<StdAtomicUsize>);

impl Drop for UsesShuttleOnDrop {
    fn drop(&mut self) {
        let mutex = Mutex::new(0);
        *mutex.lock().unwrap() += 1;
        AtomicBool::new(false).store(true, Ordering::SeqCst);
        count(&self.0);
    }
}

/// Statics are dropped at the end of teardown, as if by the main thread.
#[test]
fn static_destructors_use_shuttle_during_teardown() {
    shuttle::lazy_static! {
        static ref STATIC: std::sync::Mutex<Option<UsesShuttleOnDrop>> = std::sync::Mutex::new(None);
    }

    check_destructors_complete(1, |counter| {
        *STATIC.lock().unwrap() = Some(UsesShuttleOnDrop(counter));
    });
}

shuttle::thread_local! {
    static LEFTOVER: std::cell::RefCell<Option<UsesShuttleOnDrop>> = const { std::cell::RefCell::new(None) };
}

shuttle::lazy_static! {
    static ref LEFTOVER_STATIC: std::sync::Mutex<Option<UsesShuttleOnDrop>> = std::sync::Mutex::new(None);
}

/// Leaves a task-local value, a static, and a task that never ran, all with destructors that use
/// Shuttle, and then fails the execution.
fn fail_with_leftovers(counter: Arc<StdAtomicUsize>) {
    let (started_tx, started_rx) = mpsc::channel();
    let c = counter.clone();
    drop(future::spawn_local(async move {
        LEFTOVER.with(|local| *local.borrow_mut() = Some(UsesShuttleOnDrop(c)));
        started_tx.send(()).unwrap();
        futures::future::pending::<()>().await;
    }));
    started_rx.recv().unwrap();
    *LEFTOVER_STATIC.lock().unwrap() = Some(UsesShuttleOnDrop(counter.clone()));
    // Nothing schedules the task before the panic, so it never runs.
    let on_drop = UsesShuttleOnDrop(counter);
    drop(future::spawn_local(async move {
        let _on_drop = on_drop;
    }));
    panic!("original failure");
}

/// A failed execution is torn down before its failure is raised. Its leftovers are leaked by
/// default, and the failure is reported as it is (rather than aborting the process).
#[test]
#[should_panic(expected = "original failure")]
fn failed_execution_with_leftovers_reports_its_failure() {
    check_random(|| fail_with_leftovers(Arc::new(StdAtomicUsize::new(0))), ITERATIONS);
}

/// The same with `ContinuationFunctionBehavior::Drop`: the leftovers are dropped, as their tasks,
/// and the failure is still the one that gets reported, even if a destructor panics too.
#[test]
fn failed_execution_drops_its_leftovers_if_configured() {
    let completed = Arc::new(StdAtomicUsize::new(0));
    let counter = completed.clone();
    let mut config = Config::new();
    config.ungraceful_shutdown_config.continuation_function_behavior = ContinuationFunctionBehavior::Drop;
    let failure = panic::catch_unwind(AssertUnwindSafe(|| {
        Runner::new(RandomScheduler::new(ITERATIONS), config).run(move || {
            // Whether or not this task gets to run, it is a leftover whose destructor panics: if it
            // runs, it waits forever.
            let on_drop = OnDrop(Some(|| panic!("a destructor panicked too")));
            drop(future::spawn_local(async move {
                let _on_drop = on_drop;
                futures::future::pending::<()>().await;
            }));
            fail_with_leftovers(counter.clone());
        })
    }))
    .expect_err("the execution fails");
    assert_eq!(failure.downcast_ref::<&str>(), Some(&"original failure"));
    assert_eq!(completed.load(StdOrdering::SeqCst), 3);
}

/// A deadlock is reported as a deadlock, whatever the unfinished tasks own.
#[test]
#[should_panic(expected = "deadlock")]
fn deadlocked_execution_with_leftovers_reports_the_deadlock() {
    check_random(
        || {
            let counter = Arc::new(StdAtomicUsize::new(0));
            *LEFTOVER_STATIC.lock().unwrap() = Some(UsesShuttleOnDrop(counter.clone()));
            let (tx, rx) = mpsc::channel::<()>();
            let _tx = tx;
            let _ = rx.recv();
        },
        ITERATIONS,
    );
}
