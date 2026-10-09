//! Tests for execution teardown: dropping what the tasks that have not finished leave behind when an
//! execution ends.
//!
//! Most tests leave tasks that own something with a destructor unfinished, let the execution end,
//! and check what that destructor can do while teardown drops the task.

use shuttle::current::{self, TaskId};
use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use shuttle::future::{self, block_on, yield_now, JoinError};
use shuttle::scheduler::RandomScheduler;
use shuttle::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use shuttle::sync::{mpsc, Barrier, Condvar, Mutex, RwLock};
use shuttle::{check_random, thread, Config, ContinuationFunctionBehavior, FailurePersistence, MaxSteps, Runner};
use std::any::Any;
use std::future::Future;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool as StdAtomicBool, AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};
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

fn on_drop(f: impl FnOnce()) -> OnDrop<impl FnOnce()> {
    OnDrop(Some(f))
}

/// An `OnDrop` of any closure.
type BoxedOnDrop = OnDrop<Box<dyn FnOnce() + Send>>;

/// Where a task is when the execution ends.
#[derive(Clone, Copy, Debug)]
enum Wait {
    /// Parked between polls, the usual state of an unfinished future task.
    Parked,
    /// In the middle of `poll`, spinning on an atomic, so that teardown has to unwind its stack.
    Spinning,
}

impl Wait {
    async fn until_teardown(self) {
        match self {
            Wait::Parked => futures::future::pending::<()>().await,
            Wait::Spinning => {
                let spins = AtomicUsize::new(0);
                loop {
                    spins.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    }
}

/// Spawns and detaches a task that runs `task` and then waits for the end of the execution as
/// `wait` says. Returns once `task` has got to that point. `task` gets a `Started` to call once it
/// has set things up.
fn spawn_until_teardown<F>(wait: Wait, task: impl FnOnce(Started) -> F + 'static)
where
    F: Future<Output = ()> + 'static,
{
    let (started_tx, started_rx) = mpsc::channel();
    drop(future::spawn_local(async move {
        task(Started(started_tx)).await;
        wait.until_teardown().await;
    }));
    started_rx.recv().unwrap();
}

/// See `spawn_until_teardown`.
struct Started(mpsc::Sender<()>);

impl Started {
    fn now(&self) {
        self.0.send(()).unwrap();
    }
}

/// Spawns and detaches a task that owns `on_drop` and then waits for the end of the execution as
/// `wait` says. Returns once the task has started.
fn park(wait: Wait, on_drop: impl FnOnce() + 'static) {
    let (started_tx, started_rx) = mpsc::channel();
    drop(future::spawn_local(async move {
        let _on_drop = OnDrop(Some(on_drop));
        started_tx.send(()).unwrap();
        wait.until_teardown().await;
    }));
    started_rx.recv().unwrap();
}

fn park_until_teardown(on_drop: impl FnOnce() + 'static) {
    park(Wait::Parked, on_drop)
}

/// Spawns and detaches a task that locks `mutex` and holds it until the end of the execution, as
/// `wait` says.
fn hold_until_teardown(wait: Wait, mutex: Arc<Mutex<usize>>) {
    spawn_until_teardown(wait, move |started| async move {
        let _guard = mutex.lock().unwrap();
        started.now();
        wait.until_teardown().await;
    });
}

/// Checks that each of `num_drops` destructors that `test` sets up ran to completion in every one
/// of `iterations` executions under `config`, by giving `test` a counter to increment.
fn check_destructors_complete_with(
    config: Config,
    iterations: usize,
    num_drops: usize,
    test: impl Fn(Arc<StdAtomicUsize>) + Send + Sync + 'static,
) {
    let completed = Arc::new(StdAtomicUsize::new(0));
    let counter = completed.clone();
    Runner::new(RandomScheduler::new(iterations), config).run(move || test(counter.clone()));
    assert_eq!(completed.load(StdOrdering::SeqCst), num_drops * iterations);
}

fn check_destructors_complete(num_drops: usize, test: impl Fn(Arc<StdAtomicUsize>) + Send + Sync + 'static) {
    check_destructors_complete_with(Config::new(), ITERATIONS, num_drops, test)
}

fn count(counter: &Arc<StdAtomicUsize>) {
    counter.fetch_add(1, StdOrdering::SeqCst);
}

/// Runs `f`, which is expected to panic, and returns the panic's message.
fn panic_message<T>(f: impl FnOnce() -> T) -> String {
    let Err(payload) = panic::catch_unwind(AssertUnwindSafe(f)) else {
        panic!("expected a panic");
    };
    message_of(&*payload)
}

/// The message of a panic's payload.
fn message_of(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "<not a string>".into())
}

/// Runs `f` on a thread of its own, and returns the message of its panic, if it panics. Teardown
/// leaks the stack of a task that can't finish unwinding a panic, which leaves the thread panicking.
fn panic_message_on_own_thread(f: impl FnOnce() + Send + 'static) -> Option<String> {
    let thread = std::thread::spawn(move || panic::catch_unwind(AssertUnwindSafe(f)).map_err(|p| message_of(&*p)));
    thread.join().unwrap().err()
}

/// The destructors use locks and semaphores.
fn use_locks(counter: Arc<StdAtomicUsize>, wait: Wait) {
    let mutex = Arc::new(Mutex::new(0));
    let (m, c) = (mutex.clone(), counter.clone());
    park(wait, move || {
        *m.lock().unwrap() += 1;
        drop(m.try_lock().unwrap());
        count(&c);
    });
    let rwlock = Arc::new(RwLock::new(0));
    let c = counter.clone();
    park(wait, move || {
        *rwlock.write().unwrap() += 1;
        assert_eq!(*rwlock.read().unwrap(), 1);
        count(&c);
    });
    let semaphore = BatchSemaphore::new(1, Fairness::StrictlyFair);
    park(wait, move || {
        semaphore.try_acquire(1).unwrap();
        assert!(semaphore.try_acquire(1).is_err());
        semaphore.release(1);
        count(&counter);
    });
}

#[test]
fn destructors_use_locks_during_teardown() {
    check_destructors_complete(3, |counter| use_locks(counter, Wait::Parked));
}

#[test]
fn destructors_use_locks_while_unwinding_a_task_during_teardown() {
    check_destructors_complete(3, |counter| use_locks(counter, Wait::Spinning));
}

/// The destructors use atomics, channels, condition variables and barriers.
fn use_atomics_and_channels(counter: Arc<StdAtomicUsize>, wait: Wait) {
    let flag = Arc::new(AtomicBool::new(false));
    let c = counter.clone();
    park(wait, move || {
        flag.store(true, Ordering::SeqCst);
        assert!(flag.swap(false, Ordering::SeqCst));
        count(&c);
    });
    let (tx, rx) = mpsc::channel();
    let c = counter.clone();
    park(wait, move || {
        tx.send(1).unwrap();
        assert_eq!(rx.try_recv(), Ok(1));
        count(&c);
    });
    let condvar = Arc::new(Condvar::new());
    let barrier = Barrier::new(1);
    park(wait, move || {
        condvar.notify_one();
        condvar.notify_all();
        assert!(barrier.wait().is_leader());
        count(&counter);
    });
}

#[test]
fn destructors_use_atomics_and_channels_during_teardown() {
    check_destructors_complete(3, |counter| use_atomics_and_channels(counter, Wait::Parked));
}

#[test]
fn destructors_use_atomics_and_channels_while_unwinding_a_task_during_teardown() {
    check_destructors_complete(3, |counter| use_atomics_and_channels(counter, Wait::Spinning));
}

/// The destructors use tasks: they spawn and abort tasks, yield, and look at the current task.
fn use_tasks(counter: Arc<StdAtomicUsize>, wait: Wait) {
    let abort_handle = future::spawn(futures::future::pending::<()>()).abort_handle();
    let c = counter.clone();
    park(wait, move || {
        // Teardown has dropped the task already: it drops parked tasks in the order they were
        // created, and before it unwinds any stack.
        assert!(abort_handle.is_finished());
        abort_handle.abort();
        let _ = current::me();
        let _ = thread::current().id();
        thread::yield_now();
        assert_eq!(block_on(async { 1 }), 1);
        count(&c);
    });
    park(wait, move || {
        drop(future::spawn(async {}));
        drop(thread::spawn(|| {}));
        count(&counter);
    });
}

#[test]
fn destructors_use_tasks_during_teardown() {
    check_destructors_complete(2, |counter| use_tasks(counter, Wait::Parked));
}

#[test]
fn destructors_use_tasks_while_unwinding_a_task_during_teardown() {
    check_destructors_complete(2, |counter| use_tasks(counter, Wait::Spinning));
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
            let on_drop = on_drop(move || count(&counter));
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
            for wait in [Wait::Parked, Wait::Spinning] {
                let ids = ids_clone.clone();
                spawn_until_teardown(wait, move |started| async move {
                    let me = current::me();
                    let _on_drop = on_drop(move || ids.lock().unwrap().push((me, current::me())));
                    started.now();
                    wait.until_teardown().await;
                });
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
/// the destructors of other tasks can take it, whichever task teardown drops first: a destructor
/// that has to wait for the lock waits.
#[test]
fn lock_held_by_task_is_released_during_teardown() {
    for holder_first in [true, false] {
        for (holder, waiter) in [
            (Wait::Parked, Wait::Parked),
            (Wait::Parked, Wait::Spinning),
            (Wait::Spinning, Wait::Parked),
        ] {
            check_destructors_complete(1, move |counter| {
                let mutex = Arc::new(Mutex::new(0));
                let m = mutex.clone();
                let lock_on_drop = move || {
                    *m.lock().unwrap() += 1;
                    count(&counter);
                };
                if holder_first {
                    hold_until_teardown(holder, mutex);
                    park(waiter, lock_on_drop);
                } else {
                    park(waiter, lock_on_drop);
                    hold_until_teardown(holder, mutex);
                }
            });
        }
    }
}

/// The same for a strictly fair `BatchSemaphore`, whose permits go to its waiters in order:
/// destructors that queue for a permit that a later task holds get it one after the other.
#[test]
fn semaphore_permits_reach_queued_destructors_during_teardown() {
    check_destructors_complete(2, |counter| {
        let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::StrictlyFair));
        for _ in 0..2 {
            let (s, c) = (semaphore.clone(), counter.clone());
            park_until_teardown(move || {
                s.acquire_blocking(1).unwrap();
                s.release(1);
                count(&c);
            });
        }
        spawn_until_teardown(Wait::Parked, move |started| async move {
            semaphore.acquire(1).await.unwrap();
            let _release = on_drop(|| semaphore.release(1));
            started.now();
            futures::future::pending::<()>().await;
        });
    });
}

/// A destructor that ends a reservation of an unfair `BatchSemaphore` wakes the requests that the
/// reservation kept waiting, as in a running execution, also while teardown unwinds a stack.
#[test]
fn ending_a_reservation_during_teardown_wakes_the_waiters() {
    for wait in [Wait::Parked, Wait::Spinning] {
        check_destructors_complete(1, move |counter| {
            park(wait, move || {
                let semaphore = BatchSemaphore::new(2, Fairness::Unfair);
                block_on(semaphore.acquire(1)).unwrap();
                let mut reserve = Some(Box::pin(semaphore.acquire_reserving(1, 2)));
                let mut request = Box::pin(semaphore.acquire(1));
                let mut reserved = false;
                block_on(futures::future::poll_fn(|cx| {
                    if !reserved {
                        reserved = true;
                        assert!(reserve.as_mut().unwrap().as_mut().poll(cx).is_pending());
                        assert!(request.as_mut().poll(cx).is_pending());
                        // Only the wake of `request` gets this polled again.
                        reserve = None;
                        return std::task::Poll::Pending;
                    }
                    request.as_mut().poll(cx)
                }))
                .unwrap();
                semaphore.release(2);
                count(&counter);
            });
        });
    }
}

/// A lock that a task holds while teardown unwinds the task's stack isn't poisoned by it: that is
/// no panic. Here a cleanup guard on the same stack takes the lock again as it unwinds.
#[test]
fn lock_held_while_teardown_unwinds_its_task_is_not_poisoned() {
    check_destructors_complete(2, |counter| {
        let mutex = Arc::new(Mutex::new(0));
        let rwlock = Arc::new(RwLock::new(0));
        let (m, r, c) = (mutex.clone(), rwlock.clone(), counter.clone());
        spawn_until_teardown(Wait::Parked, move |started| async move {
            let (m1, r1) = (m.clone(), r.clone());
            let _cleanup = on_drop(move || {
                *m1.lock().unwrap() += 1;
                *r1.write().unwrap() += 1;
                count(&c);
            });
            started.now();
            let flag = AtomicBool::new(false);
            loop {
                let _m = m.lock().unwrap();
                let _r = r.write().unwrap();
                // A scheduling point while holding both locks.
                flag.store(true, Ordering::SeqCst);
            }
        });
        park_until_teardown(move || {
            assert!(mutex.lock().is_ok());
            assert!(rwlock.write().is_ok());
            count(&counter);
        });
    });
}

/// A task that teardown unwinds from inside `MutexGuard::drop`, where it was about to release the
/// lock, still releases it.
#[test]
fn lock_released_by_a_task_that_teardown_unwinds_is_free() {
    check_destructors_complete_with(Config::new(), 100, 1, |counter| {
        let mutex = Arc::new(Mutex::new(0));
        let m = mutex.clone();
        spawn_until_teardown(Wait::Parked, move |started| async move {
            started.now();
            loop {
                drop(m.lock().unwrap());
            }
        });
        // Waits for teardown to unwind that task.
        park_until_teardown(move || {
            drop(mutex.lock().unwrap());
            count(&counter);
        });
    });
}

/// Teardown unwinds a task blocked in `Condvar::wait`, whose destructor then notifies the condvar.
#[test]
fn destructor_of_task_unwound_while_waiting_on_a_condvar_can_notify_it() {
    check_destructors_complete(1, |counter| {
        let condvar = Arc::new(Condvar::new());
        let mutex = Arc::new(Mutex::new(()));
        spawn_until_teardown(Wait::Parked, move |started| async move {
            let cv = condvar.clone();
            let _on_drop = on_drop(move || {
                cv.notify_all();
                cv.notify_one();
                count(&counter);
            });
            let guard = mutex.lock().unwrap();
            started.now();
            let _guard = condvar.wait(guard).unwrap();
        });
    });
}

/// Teardown unwinds a task blocked sending on a full channel. A destructor that runs afterwards
/// sees the channel without that sender.
#[test]
fn channel_is_consistent_after_teardown_unwinds_a_blocked_sender() {
    check_destructors_complete(1, |counter| {
        let (tx, rx) = mpsc::sync_channel::<u32>(1);
        tx.send(1).unwrap();
        let blocked_tx = tx.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let sender = future::spawn_local(async move {
            started_tx.send(()).unwrap();
            // Blocks: the channel is full.
            let _ = blocked_tx.send(2);
        })
        .abort_handle();
        started_rx.recv().unwrap();
        park_until_teardown(move || {
            // Teardown unwinds the sender once no other task can run.
            while !sender.is_finished() {
                thread::yield_now();
            }
            assert_eq!(rx.try_recv(), Ok(1));
            assert_eq!(tx.try_send(3), Ok(()));
            assert_eq!(rx.try_recv(), Ok(3));
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

    for wait in [Wait::Parked, Wait::Spinning] {
        check_destructors_complete(1, move |counter| {
            spawn_until_teardown(wait, move |started| async move {
                LOCAL.with(|local| *local.borrow_mut() = Some(Local(counter, current::me())));
                started.now();
                wait.until_teardown().await;
            });
        });
    }
}

/// A task whose stack teardown unwinds, and that catches the unwind, is unwound again where it next
/// reaches a scheduling point, rather than going on.
#[test]
fn task_that_catches_the_unwind_of_its_stack_is_unwound_again() {
    let went_on = Arc::new(StdAtomicBool::new(false));
    let w = went_on.clone();
    check_destructors_complete(1, move |counter| {
        let went_on = w.clone();
        let lock = Arc::new(Mutex::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        drop(future::spawn_local(async move {
            let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                started_tx.send(()).unwrap();
                // In the middle of `poll` when the execution ends.
                let spins = AtomicUsize::new(0);
                loop {
                    spins.fetch_add(1, Ordering::SeqCst);
                }
            }));
            // Uses a lock as the unwind drops it.
            let _on_drop = on_drop(move || {
                *lock.lock().unwrap() += 1;
                count(&counter);
            });
            let flag = AtomicBool::new(false);
            flag.load(Ordering::SeqCst);
            went_on.store(true, StdOrdering::SeqCst);
            let (tx, rx) = mpsc::channel::<()>();
            let _tx = tx;
            let _ = rx.recv();
        }));
        started_rx.recv().unwrap();
    });
    assert!(!went_on.load(StdOrdering::SeqCst));
}

/// A task-local value that a future sets after its task finished is dropped as that task too.
#[test]
fn task_local_set_after_its_task_finished_is_dropped_as_the_task() {
    shuttle::thread_local! {
        static LATE: std::cell::RefCell<Option<OnDrop<Box<dyn FnOnce()>>>> = const { std::cell::RefCell::new(None) };
    }

    /// A future whose destructor sets a task-local value. The task drops it once it has dropped
    /// its task-local values.
    struct SetsLocalOnDrop(Option<Arc<StdAtomicUsize>>);

    impl Future for SetsLocalOnDrop {
        type Output = ();

        fn poll(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
            std::task::Poll::Ready(())
        }
    }

    impl Drop for SetsLocalOnDrop {
        fn drop(&mut self) {
            let counter = self.0.take().unwrap();
            let me = current::me();
            let local: Box<dyn FnOnce()> = Box::new(move || {
                assert_eq!(current::me(), me);
                drop(Mutex::new(()).lock().unwrap());
                count(&counter);
            });
            LATE.with(|late| *late.borrow_mut() = Some(OnDrop(Some(local))));
        }
    }

    check_destructors_complete(1, |counter| {
        future::block_on(future::spawn(SetsLocalOnDrop(Some(counter)))).unwrap();
    });
}

/// A panic in a destructor during teardown fails the test like any other panic (rather than
/// aborting the process).
#[test]
#[should_panic(expected = "destructor failed")]
fn panic_in_destructor_during_teardown_fails_test() {
    check_random(|| park_until_teardown(|| panic!("destructor failed")), ITERATIONS);
}

/// A destructor that blocks, and can never be woken, fails the test like a deadlock.
#[test]
#[should_panic(expected = "deadlock while dropping the tasks that were unfinished at the end of the execution")]
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

/// The same while the destructor holds a lock (rather than aborting the process), and the task's
/// other destructors still run.
#[test]
fn blocking_in_destructor_that_holds_a_lock_fails_test() {
    shuttle::thread_local! {
        static LOCAL: std::cell::RefCell<Option<OnDrop<Box<dyn FnOnce()>>>> = const { std::cell::RefCell::new(None) };
    }

    let completed = Arc::new(StdAtomicUsize::new(0));
    let counter = completed.clone();
    let message = panic_message(move || {
        check_random(
            move || {
                let mutex = Arc::new(Mutex::new(0));
                let (tx, rx) = mpsc::channel::<()>();
                let counter = counter.clone();
                spawn_until_teardown(Wait::Parked, move |started| async move {
                    let local: Box<dyn FnOnce()> = Box::new(move || {
                        drop(Mutex::new(()).lock().unwrap());
                        count(&counter);
                    });
                    LOCAL.with(|l| *l.borrow_mut() = Some(OnDrop(Some(local))));
                    let _on_drop = on_drop(move || {
                        let _tx = tx;
                        let _guard = mutex.lock().unwrap();
                        let _ = rx.recv();
                    });
                    started.now();
                    futures::future::pending::<()>().await;
                });
            },
            1,
        )
    });
    assert!(message.contains("deadlock while dropping"), "{message}");
    assert_eq!(completed.load(StdOrdering::SeqCst), 1);
}

/// A destructor that keeps running, here waiting for a flag that nothing sets, fails the test when
/// it exceeds the step bound, rather than hanging.
#[test]
#[should_panic(expected = "exceeded the step bound (1000) while it was being dropped at the end of the execution")]
fn spinning_destructor_fails_test() {
    let mut config = Config::new();
    config.max_steps = MaxSteps::FailAfter(1000);
    Runner::new(RandomScheduler::new(1), config).run(|| {
        let flag = AtomicBool::new(false);
        park_until_teardown(move || {
            while !flag.load(Ordering::SeqCst) {
                thread::yield_now();
            }
        });
    });
}

/// Destructors that keep spawning tasks whose destructors spawn again exceed the step bound too.
#[test]
#[should_panic(expected = "exceeded the step bound (1000)")]
fn respawning_destructors_fail_test() {
    struct Respawn;

    impl Drop for Respawn {
        fn drop(&mut self) {
            let respawn = Respawn;
            drop(future::spawn_local(async move {
                let _respawn = respawn;
            }));
        }
    }

    let mut config = Config::new();
    config.max_steps = MaxSteps::FailAfter(1000);
    Runner::new(RandomScheduler::new(1), config).run(|| park_until_teardown(|| drop(Respawn)));
}

/// A destructor that waits for another task's destructor by yielding lets that one run.
#[test]
fn destructor_waits_for_another_destructor_by_yielding() {
    check_destructors_complete(2, |counter| {
        let flag = Arc::new(AtomicBool::new(false));
        let (f, c) = (flag.clone(), counter.clone());
        park_until_teardown(move || {
            while !f.load(Ordering::SeqCst) {
                thread::yield_now();
            }
            count(&c);
        });
        park_until_teardown(move || {
            flag.store(true, Ordering::SeqCst);
            count(&counter);
        });
    });
}

/// Handles of the tasks that teardown drops report them cancelled. The tasks here are spawned by
/// destructors: a task whose handle is still around when an execution ends has finished, since the
/// execution waits for it.
#[test]
fn handles_of_torn_down_tasks_report_them_cancelled() {
    check_destructors_complete(3, |counter| {
        let c = counter.clone();
        park_until_teardown(move || {
            let task = future::spawn(async {});
            assert!(matches!(block_on(task), Err(JoinError::Cancelled)));
            count(&c);
        });
        let c = counter.clone();
        park_until_teardown(move || {
            let task = future::spawn(async {});
            while !task.is_finished() {
                thread::yield_now();
            }
            task.abort();
            assert!(matches!(block_on(task), Err(JoinError::Cancelled)));
            count(&c);
        });
        park_until_teardown(move || {
            assert!(thread::spawn(|| {}).join().is_err());
            count(&counter);
        });
    });
}

/// A scoped thread spawned during teardown would never run, so the scope couldn't return: that
/// fails the test, rather than leaving the thread to outlive the scope.
#[test]
#[should_panic(expected = "a destructor spawned a scoped thread while the execution was being torn down")]
fn scoped_thread_spawned_during_teardown_fails_test() {
    check_random(
        || {
            park_until_teardown(|| {
                let value = 1;
                thread::scope(|s| {
                    s.spawn(|| assert_eq!(value, 1));
                });
            })
        },
        1,
    );
}

/// `current::context_switches` counts the scheduling points of destructors too.
#[test]
fn context_switches_advance_during_teardown() {
    check_destructors_complete(1, |counter| {
        park_until_teardown(move || {
            let before = current::context_switches();
            *Mutex::new(0).lock().unwrap() += 1;
            AtomicBool::new(false).store(true, Ordering::SeqCst);
            assert!(current::context_switches() > before);
            count(&counter);
        })
    });
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

/// Statics are dropped at the end of teardown, as if by the main thread, after the unfinished
/// tasks.
#[test]
fn static_destructors_use_shuttle_during_teardown() {
    struct Static(Arc<StdAtomicUsize>, Arc<AtomicBool>);

    impl Drop for Static {
        fn drop(&mut self) {
            assert_eq!(current::me(), TaskId::from(0));
            assert!(self.1.load(Ordering::SeqCst), "the tasks are dropped first");
            drop(UsesShuttleOnDrop(self.0.clone()));
        }
    }

    shuttle::lazy_static! {
        static ref STATIC: std::sync::Mutex<Option<Static>> = std::sync::Mutex::new(None);
    }

    check_destructors_complete(1, |counter| {
        let task_dropped = Arc::new(AtomicBool::new(false));
        *STATIC.lock().unwrap() = Some(Static(counter, task_dropped.clone()));
        park_until_teardown(move || task_dropped.store(true, Ordering::SeqCst));
    });
}

/// A static's destructor can set task-local values of the main thread's, which are dropped with it.
#[test]
fn static_destructor_sets_task_local_value() {
    shuttle::thread_local! {
        static LOCAL: std::cell::RefCell<Option<UsesShuttleOnDrop>> = const { std::cell::RefCell::new(None) };
    }

    struct SetsLocal(Arc<StdAtomicUsize>);

    impl Drop for SetsLocal {
        fn drop(&mut self) {
            let counter = self.0.clone();
            LOCAL.with(|local| *local.borrow_mut() = Some(UsesShuttleOnDrop(counter)));
        }
    }

    shuttle::lazy_static! {
        static ref STATIC: std::sync::Mutex<Option<SetsLocal>> = std::sync::Mutex::new(None);
    }

    check_destructors_complete(1, |counter| *STATIC.lock().unwrap() = Some(SetsLocal(counter)));
}

/// A static's destructor that blocks fails the test, and says that it was a static's.
#[test]
#[should_panic(expected = "A static's destructor, dropped as main-thread, blocked")]
fn blocking_static_destructor_fails_test() {
    shuttle::lazy_static! {
        static ref STATIC: std::sync::Mutex<Option<BoxedOnDrop>> = std::sync::Mutex::new(None);
    }

    check_random(
        || {
            let (tx, rx) = mpsc::channel::<()>();
            let rx = std::sync::Mutex::new(rx);
            *STATIC.lock().unwrap() = Some(OnDrop(Some(Box::new(move || {
                let _tx = tx;
                let _ = rx.lock().unwrap().recv();
            }))));
        },
        1,
    );
}

/// A waker can outlive its execution, and name a task that a later execution doesn't have. Waking
/// it during teardown does nothing.
#[test]
fn waker_from_another_execution_does_nothing_during_teardown() {
    static WAKER: std::sync::Mutex<Option<std::task::Waker>> = std::sync::Mutex::new(None);
    let executions = Arc::new(StdAtomicUsize::new(0));
    check_random(
        move || {
            if executions.fetch_add(1, StdOrdering::SeqCst) == 0 {
                // The waker of the last of these tasks names a task that the next execution lacks.
                for _ in 0..5 {
                    let waker = future::spawn(futures::future::poll_fn(|cx| {
                        std::task::Poll::Ready(cx.waker().clone())
                    }));
                    *WAKER.lock().unwrap() = Some(block_on(waker).unwrap());
                }
            } else {
                park_until_teardown(|| WAKER.lock().unwrap().take().unwrap().wake());
            }
        },
        2,
    );
}

shuttle::thread_local! {
    static LEFTOVER: std::cell::RefCell<Option<UsesShuttleOnDrop>> = const { std::cell::RefCell::new(None) };
}

shuttle::lazy_static! {
    static ref LEFTOVER_STATIC: std::sync::Mutex<Option<UsesShuttleOnDrop>> = std::sync::Mutex::new(None);
}

/// Leaves a task-local value, a static, a parked task, and a task that never ran, each with a value
/// whose destructor uses Shuttle, and then fails the execution. If `panic_too`, the parked task and
/// the task that never ran also own values whose destructors panic.
fn fail_with_leftovers(counter: Arc<StdAtomicUsize>, panic_too: bool) {
    let c = counter.clone();
    spawn_until_teardown(Wait::Parked, move |started| async move {
        LEFTOVER.with(|local| *local.borrow_mut() = Some(UsesShuttleOnDrop(c)));
        started.now();
    });
    *LEFTOVER_STATIC.lock().unwrap() = Some(UsesShuttleOnDrop(counter.clone()));
    let c = counter.clone();
    park_until_teardown(move || {
        drop(UsesShuttleOnDrop(c));
        if panic_too {
            panic!("a destructor panicked too");
        }
    });
    // Nothing schedules the task before the panic, so it never runs.
    let never_ran = (
        UsesShuttleOnDrop(counter),
        panic_too.then(|| on_drop(|| panic!("a destructor panicked too"))),
    );
    drop(future::spawn_local(async move {
        let _never_ran = never_ran;
    }));
    panic!("original failure");
}

/// A failed execution is torn down before its failure is raised, which is reported as it is. Its
/// task-local values and statics are dropped, and by default, its parked futures and the functions
/// of the tasks that never ran are leaked.
#[test]
fn failed_execution_with_leftovers_reports_its_failure() {
    let completed = Arc::new(StdAtomicUsize::new(0));
    let counter = completed.clone();
    let message = panic_message(move || check_random(move || fail_with_leftovers(counter.clone(), false), ITERATIONS));
    assert_eq!(message, "original failure");
    // `check_random` stops at the first failure.
    assert_eq!(completed.load(StdOrdering::SeqCst), 2);
}

/// The same with `ContinuationFunctionBehavior::Drop`: the parked futures and the functions of the
/// tasks that never ran are dropped too, as their tasks, and the failure is still the one that gets
/// reported, even if a destructor panics too.
#[test]
fn failed_execution_drops_its_leftovers_if_configured() {
    let completed = Arc::new(StdAtomicUsize::new(0));
    let counter = completed.clone();
    let mut config = Config::new();
    config.ungraceful_shutdown_config.continuation_function_behavior = ContinuationFunctionBehavior::Drop;
    let message = panic_message(move || {
        Runner::new(RandomScheduler::new(ITERATIONS), config).run(move || fail_with_leftovers(counter.clone(), true))
    });
    assert_eq!(message, "original failure");
    // The runner stops at the first failure.
    assert_eq!(completed.load(StdOrdering::SeqCst), 4);
}

/// A failed execution's statics with plain destructors are dropped.
#[test]
fn failed_execution_drops_its_statics() {
    static DROPS: StdAtomicUsize = StdAtomicUsize::new(0);

    struct Plain;

    impl Drop for Plain {
        fn drop(&mut self) {
            DROPS.fetch_add(1, StdOrdering::SeqCst);
        }
    }

    shuttle::lazy_static! {
        static ref STATIC: std::sync::Mutex<Option<Plain>> = std::sync::Mutex::new(None);
    }

    let message = panic_message(|| {
        check_random(
            || {
                *STATIC.lock().unwrap() = Some(Plain);
                panic!("original failure");
            },
            ITERATIONS,
        )
    });
    assert_eq!(message, "original failure");
    assert_eq!(DROPS.load(StdOrdering::SeqCst), 1);
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

/// Labels whose destructors look up task names, while teardown drops the labels, don't hide the
/// failure.
#[test]
#[should_panic(expected = "deadlock")]
fn label_destructors_that_look_up_task_names_during_teardown() {
    #[derive(Clone, Debug)]
    struct NamesTaskOnDrop(TaskId);

    impl Drop for NamesTaskOnDrop {
        fn drop(&mut self) {
            let _ = format!("{:?}", self.0);
        }
    }

    check_random(
        || {
            current::set_label_for_task(current::me(), NamesTaskOnDrop(current::me()));
            let (tx, rx) = mpsc::channel::<()>();
            let _tx = tx;
            let _ = rx.recv();
        },
        1,
    );
}

/// A detached task that panics, and switches out while it unwinds (here, to release a lock), fails
/// the test, even if the execution could end before the task resumes.
#[test]
fn panic_of_detached_task_that_switches_while_unwinding_fails_test() {
    static PANICS: StdAtomicUsize = StdAtomicUsize::new(0);
    let mut failures = 0;
    for seed in 0..50 {
        let result = panic::catch_unwind(|| {
            shuttle::check_random_with_seed(
                || {
                    let mutex = Arc::new(Mutex::new(0));
                    drop(future::spawn(async move {
                        let _guard = mutex.lock().unwrap();
                        PANICS.fetch_add(1, StdOrdering::SeqCst);
                        panic!("detached task failed");
                    }));
                    thread::yield_now();
                },
                seed,
                1,
            )
        });
        if let Err(payload) = result {
            assert_eq!(payload.downcast_ref::<&str>(), Some(&"detached task failed"));
            failures += 1;
        }
        assert!(!std::thread::panicking(), "a panic was left unwinding");
    }
    assert!(failures > 0);
    assert_eq!(failures, PANICS.load(StdOrdering::SeqCst));
}

/// The same for a task whose `JoinHandle` is dropped while it unwinds.
#[test]
fn panic_of_task_detached_while_it_unwinds_fails_test() {
    static PANICS: StdAtomicUsize = StdAtomicUsize::new(0);
    let mut failures = 0;
    for seed in 0..50 {
        let result = panic::catch_unwind(|| {
            shuttle::check_random_with_seed(
                || {
                    let mutex = Arc::new(Mutex::new(0));
                    let task = future::spawn(async move {
                        let _guard = mutex.lock().unwrap();
                        PANICS.fetch_add(1, StdOrdering::SeqCst);
                        panic!("task failed");
                    });
                    for _ in 0..3 {
                        thread::yield_now();
                    }
                    drop(task);
                },
                seed,
                1,
            )
        });
        if let Err(payload) = result {
            assert_eq!(payload.downcast_ref::<&str>(), Some(&"task failed"));
            failures += 1;
        }
        assert!(!std::thread::panicking(), "a panic was left unwinding");
    }
    assert!(failures > 0);
    assert_eq!(failures, PANICS.load(StdOrdering::SeqCst));
}

/// A task that catches a panic, whose unwind switches out (to release a lock), doesn't affect a
/// detached task that blocks meanwhile: `std::thread::panicking()` is true for every task while one
/// unwinds, as they share the OS thread.
#[test]
fn caught_panic_does_not_attach_other_tasks() {
    for seed in 0..100 {
        shuttle::check_random_with_seed(
            || {
                // A detached task that waits forever.
                let (tx, rx) = mpsc::channel::<()>();
                drop(future::spawn(async move {
                    let _tx = tx;
                    let _ = rx.recv();
                }));
                let mutex = Arc::new(Mutex::new(0));
                let result = panic::catch_unwind(AssertUnwindSafe(|| {
                    let _guard = mutex.lock().unwrap();
                    panic!("expected panic");
                }));
                assert!(result.is_err());
            },
            seed,
            1,
        );
    }
}

/// A task that panics, and is switched out while it unwinds when a stopped execution stops, finishes
/// unwinding during teardown, and its panic fails the test.
#[test]
fn panic_of_task_unwinding_when_the_execution_stops_fails_test() {
    static PANICS: StdAtomicUsize = StdAtomicUsize::new(0);
    let mut failures = 0;
    for max_steps in 1..30 {
        for seed in 0..5 {
            let mut config = Config::new();
            config.max_steps = MaxSteps::ContinueAfter(max_steps);
            let result = panic::catch_unwind(|| {
                Runner::new(RandomScheduler::new_from_seed(seed, 1), config).run(|| {
                    let mutex = Arc::new(Mutex::new(0));
                    let task = thread::spawn(move || {
                        let _guard = mutex.lock().unwrap();
                        PANICS.fetch_add(1, StdOrdering::SeqCst);
                        panic!("task failed");
                    });
                    let _ = task.join();
                })
            });
            if result.is_err() {
                failures += 1;
            }
            assert!(!std::thread::panicking(), "a panic was left unwinding");
        }
    }
    assert!(failures > 0);
    assert_eq!(failures, PANICS.load(StdOrdering::SeqCst));
}

/// What a task does once it has caught its panic.
#[derive(Clone, Copy, Debug)]
enum AfterCatch {
    /// Spin, until the step bound stops the execution.
    Spin,
    /// Yield, until the step bound stops the execution.
    Yield,
    /// Block on a lock that the main thread holds for good.
    Block,
}

/// A task that is unwinding a panic when a stopped execution stops, and catches the panic, runs no
/// further during teardown than it takes to catch it, and doesn't fail the test.
#[test]
fn caught_panic_of_task_unwinding_when_the_execution_stops_does_not_fail_test() {
    // How many times the task has spun or yielded since it caught the panic.
    static TURNS: StdAtomicUsize = StdAtomicUsize::new(0);
    for after_catch in [AfterCatch::Spin, AfterCatch::Yield, AfterCatch::Block] {
        for max_steps in 1..30 {
            for seed in 0..4 {
                TURNS.store(0, StdOrdering::SeqCst);
                let mut config = Config::new();
                config.max_steps = MaxSteps::ContinueAfter(max_steps);
                Runner::new(RandomScheduler::new_from_seed(seed, 1), config).run(move || {
                    let held = Arc::new(Mutex::new(0));
                    let h = held.clone();
                    let _held = held.lock().unwrap();
                    let mutex = Arc::new(Mutex::new(0));
                    let _task = thread::spawn(move || {
                        let result = panic::catch_unwind(AssertUnwindSafe(|| {
                            let _guard = mutex.lock().unwrap();
                            panic!("expected panic");
                        }));
                        assert!(result.is_err());
                        match after_catch {
                            AfterCatch::Spin => {
                                let flag = AtomicBool::new(false);
                                while !flag.load(Ordering::SeqCst) {
                                    TURNS.fetch_add(1, StdOrdering::SeqCst);
                                }
                            }
                            AfterCatch::Yield => loop {
                                TURNS.fetch_add(1, StdOrdering::SeqCst);
                                thread::yield_now();
                            },
                            AfterCatch::Block => drop(h.lock()),
                        }
                    });
                    loop {
                        thread::yield_now();
                    }
                });
                assert!(!std::thread::panicking(), "a panic was left unwinding");
                // Only the execution's steps.
                assert!(TURNS.load(StdOrdering::SeqCst) < max_steps);
            }
        }
    }
}

/// Neither does a task that catches a panic while another task is unwinding one, and then blocks.
#[test]
fn caught_panics_when_the_execution_stops_do_not_fail_test() {
    for max_steps in 10..60 {
        for seed in 0..4 {
            let mut config = Config::new();
            config.max_steps = MaxSteps::ContinueAfter(max_steps);
            Runner::new(RandomScheduler::new_from_seed(seed, 1), config).run(|| {
                let unwinding = Arc::new(StdAtomicBool::new(false));
                let held = Arc::new(Mutex::new(0));
                let h = held.clone();
                let _held = held.lock().unwrap();
                // Catches a panic, whose unwind switches out to release a lock.
                let mutex = Arc::new(Mutex::new(0));
                let u = unwinding.clone();
                let _first = thread::spawn(move || {
                    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                        let _guard = mutex.lock().unwrap();
                        u.store(true, StdOrdering::SeqCst);
                        panic!("expected panic");
                    }));
                });
                // Catches a panic while the other task unwinds, and then blocks.
                let _second = thread::spawn(move || {
                    while !unwinding.load(StdOrdering::SeqCst) {
                        thread::yield_now();
                    }
                    let _ = panic::catch_unwind(|| panic!("expected panic"));
                    drop(h.lock());
                });
                loop {
                    thread::yield_now();
                }
            });
            assert!(!std::thread::panicking(), "a panic was left unwinding");
        }
    }
}

/// A detached task that catches a panic while another task is unwinding one, and then waits forever,
/// doesn't keep the execution from finishing.
#[test]
fn task_that_catches_a_panic_while_another_unwinds_one_stays_detached() {
    for seed in 0..300 {
        shuttle::check_random_with_seed(
            || {
                let unwinding = Arc::new(StdAtomicBool::new(false));
                let u = unwinding.clone();
                drop(future::spawn(async move {
                    while !u.load(StdOrdering::SeqCst) {
                        yield_now().await;
                    }
                    let _ = panic::catch_unwind(|| panic!("expected panic"));
                    // Waits forever.
                    let (tx, rx) = mpsc::channel::<()>();
                    let _tx = tx;
                    let _ = rx.recv();
                }));
                // Catches a panic, whose unwind switches out to release a lock.
                let mutex = Arc::new(Mutex::new(0));
                thread::spawn(move || {
                    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                        let _guard = mutex.lock().unwrap();
                        unwinding.store(true, StdOrdering::SeqCst);
                        panic!("expected panic");
                    }));
                })
                .join()
                .unwrap();
            },
            seed,
            1,
        );
    }
}

/// The same for a detached task whose own unwind switched out, and that catches its panic while
/// another detached task is unwinding one. Here, nothing else runs once the other task has caught
/// its panic too.
#[test]
fn task_that_catches_its_panic_while_another_unwinds_one_stays_detached() {
    for seed in 0..300 {
        shuttle::check_random_with_seed(
            || {
                let unwinding = Arc::new(StdAtomicBool::new(false));
                let u = unwinding.clone();
                // Catches a panic, whose unwind switches out to release a lock, and waits forever.
                let first = Arc::new(Mutex::new(0));
                drop(future::spawn(async move {
                    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                        let _guard = first.lock().unwrap();
                        u.store(true, StdOrdering::SeqCst);
                        panic!("expected panic");
                    }));
                    let (tx, rx) = mpsc::channel::<()>();
                    let _tx = tx;
                    let _ = rx.recv();
                }));
                // Catches a panic while the other task unwinds, whose unwind switches out too.
                let second = Arc::new(Mutex::new(0));
                drop(future::spawn(async move {
                    while !unwinding.load(StdOrdering::SeqCst) {
                        yield_now().await;
                    }
                    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                        let _guard = second.lock().unwrap();
                        panic!("expected panic");
                    }));
                }));
            },
            seed,
            1,
        );
    }
}

/// A detached task that resumes a panic with `panic::resume_unwind`, which doesn't call the panic
/// hook, and switches out while it unwinds (here, to release a lock), fails the test, even if the
/// execution could end before the task resumes.
#[test]
fn resumed_panic_of_detached_task_that_switches_while_unwinding_fails_test() {
    static RESUMED: StdAtomicUsize = StdAtomicUsize::new(0);
    let mut failures = 0;
    for seed in 0..100 {
        let result = panic::catch_unwind(|| {
            shuttle::check_random_with_seed(
                || {
                    let mutex = Arc::new(Mutex::new(0));
                    drop(future::spawn(async move {
                        let payload = panic::catch_unwind(|| panic!("task failed")).unwrap_err();
                        // A scheduling point while no panic unwinds.
                        thread::yield_now();
                        let _guard = mutex.lock().unwrap();
                        RESUMED.fetch_add(1, StdOrdering::SeqCst);
                        panic::resume_unwind(payload);
                    }));
                    for _ in 0..3 {
                        thread::yield_now();
                    }
                },
                seed,
                1,
            )
        });
        if let Err(payload) = result {
            assert_eq!(payload.downcast_ref::<&str>(), Some(&"task failed"));
            failures += 1;
        }
        assert!(!std::thread::panicking(), "a panic was left unwinding");
    }
    assert!(failures > 0);
    assert_eq!(failures, RESUMED.load(StdOrdering::SeqCst));
}

/// A task that is unwinding a panic when a stopped execution stops, and whose unwind blocks on a lock
/// that is never released, fails the test, rather than aborting the process.
#[test]
fn task_whose_unwind_blocks_when_the_execution_stops_fails_test() {
    let mut failures = 0;
    for max_steps in 1..40 {
        for seed in 0..3 {
            let message = panic_message_on_own_thread(move || {
                let mut config = Config::new();
                config.max_steps = MaxSteps::ContinueAfter(max_steps);
                Runner::new(RandomScheduler::new_from_seed(seed, 1), config).run(|| {
                    let held = Arc::new(Mutex::new(0));
                    let h = held.clone();
                    let _held = held.lock().unwrap();
                    let mutex = Arc::new(Mutex::new(0));
                    let _task = thread::spawn(move || {
                        let _on_drop = on_drop(move || drop(h.lock()));
                        let _guard = mutex.lock().unwrap();
                        panic!("task failed");
                    });
                    loop {
                        thread::yield_now();
                    }
                });
            });
            if let Some(message) = message {
                let expected = "was unwinding a panic when the execution stopped, and blocked before it had finished";
                assert!(message.contains(expected), "{message}");
                failures += 1;
            }
        }
    }
    assert!(failures > 0);
}

/// A failed execution leaks the stack of a task that is unwinding a panic, like any stack: the rest
/// of the unwind could panic, which would abort the process. Here it would.
#[test]
fn failed_execution_leaks_the_stack_of_a_task_unwinding_a_panic() {
    for seed in 0..50 {
        let message = panic_message_on_own_thread(move || {
            shuttle::check_random_with_seed(
                || {
                    let second_failed = Arc::new(StdAtomicBool::new(false));
                    let failed = second_failed.clone();
                    let mutex = Arc::new(Mutex::new(0));
                    thread::spawn(move || {
                        let _on_drop = on_drop(move || {
                            if failed.load(StdOrdering::SeqCst) {
                                panic!("panic while unwinding a panic");
                            }
                        });
                        let _guard = mutex.lock().unwrap();
                        panic!("first task failed");
                    });
                    thread::spawn(move || {
                        thread::yield_now();
                        second_failed.store(true, StdOrdering::SeqCst);
                        panic!("second task failed");
                    });
                    loop {
                        thread::yield_now();
                    }
                },
                seed,
                1,
            )
        });
        let message = message.expect("a task failed");
        assert!(message.ends_with("task failed"), "{message}");
    }
}

/// A detached task that panics while another task is unwinding a panic can't tell its own panic from
/// the other one. If it switches out while it unwinds, and is still unwinding when the execution
/// ends, that fails the test, since its panic would be lost otherwise.
#[test]
fn panic_of_detached_task_while_another_task_unwinds_fails_test() {
    let mut lost = 0;
    for seed in 0..500 {
        let message = panic_message_on_own_thread(move || {
            shuttle::check_random_with_seed(
                || {
                    let unwinding = Arc::new(StdAtomicBool::new(false));
                    let u = unwinding.clone();
                    // Catches a panic, whose unwind switches out to release a lock.
                    let first = Arc::new(Mutex::new(0));
                    let task = thread::spawn(move || {
                        let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                            let _guard = first.lock().unwrap();
                            u.store(true, StdOrdering::SeqCst);
                            panic!("expected panic");
                        }));
                    });
                    // Panics while the other task unwinds, and switches out as it unwinds too.
                    let second = Arc::new(Mutex::new(0));
                    drop(future::spawn(async move {
                        while !unwinding.load(StdOrdering::SeqCst) {
                            yield_now().await;
                        }
                        let _guard = second.lock().unwrap();
                        panic!("detached task failed");
                    }));
                    task.join().unwrap();
                },
                seed,
                1,
            )
        });
        match message {
            None => {}
            Some(message) if message == "detached task failed" => {}
            Some(message) => {
                assert!(message.contains("can't tell which task it is"), "{message}");
                lost += 1;
            }
        }
    }
    assert!(lost > 0);
}

/// A late panic, of a task that was unwinding a panic when a stopped execution stopped, persists the
/// schedule up to the stop, which replays it.
#[test]
fn late_panic_persists_its_schedule() {
    use shuttle::scheduler::{ReplayScheduler, Schedule};

    // How many times the main thread yielded since the task panicked, and how many times it had when
    // the task finished unwinding.
    static YIELDS: StdAtomicUsize = StdAtomicUsize::new(0);
    static YIELDS_SEEN: StdAtomicUsize = StdAtomicUsize::new(0);

    fn panics_and_yields() {
        YIELDS.store(0, StdOrdering::SeqCst);
        let panicked = Arc::new(StdAtomicBool::new(false));
        let p = panicked.clone();
        let mutex = Arc::new(Mutex::new(0));
        drop(thread::spawn(move || {
            let _seen = on_drop(|| YIELDS_SEEN.store(YIELDS.load(StdOrdering::SeqCst), StdOrdering::SeqCst));
            let _guard = mutex.lock().unwrap();
            p.store(true, StdOrdering::SeqCst);
            panic!("task failed");
        }));
        loop {
            if panicked.load(StdOrdering::SeqCst) {
                YIELDS.fetch_add(1, StdOrdering::SeqCst);
            }
            thread::yield_now();
        }
    }

    // The schedule stops while the task is unwinding, once the main thread has yielded twice.
    let schedule = Schedule::new_from_task_ids(0, vec![0, 0, 1, 1, 0, 0]);
    let directory = tempfile::tempdir().unwrap();
    let mut config = Config::new();
    config.failure_persistence = FailurePersistence::File(Some(directory.path().to_path_buf()));
    let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
    scheduler.set_allow_incomplete();
    let message = panic_message(|| Runner::new(scheduler, config).run(panics_and_yields));
    assert_eq!(message, "task failed");
    assert_eq!(YIELDS_SEEN.load(StdOrdering::SeqCst), 2);

    // The panic hook may have persisted the schedule up to the panic too, before it.
    let mut schedules = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    schedules.sort();
    let schedule = schedules.pop().expect("a persisted schedule");
    YIELDS_SEEN.store(0, StdOrdering::SeqCst);
    let mut scheduler = ReplayScheduler::new_from_file(schedule).unwrap();
    scheduler.set_allow_incomplete();
    let message = panic_message(|| Runner::new(scheduler, Config::new()).run(panics_and_yields));
    assert_eq!(message, "task failed");
    assert_eq!(YIELDS_SEEN.load(StdOrdering::SeqCst), 2);
}

/// A destructor that waits for a static to be dropped deadlocks, as it would without Shuttle, where
/// statics are never dropped. It must not be resumed after statics were dropped.
#[test]
fn destructor_that_waits_for_a_static_deadlocks() {
    struct Canary(usize);

    impl Drop for Canary {
        fn drop(&mut self) {
            self.0 = 0xDEAD;
        }
    }

    shuttle::lazy_static! {
        static ref DATA: Canary = Canary(0xC0FFEE);
        static ref LOCK: Mutex<Vec<usize>> = Mutex::new(vec![1, 2, 3]);
        static ref SENDER: std::sync::Mutex<Option<mpsc::Sender<()>>> = std::sync::Mutex::new(None);
    }

    let message = panic_message(|| {
        check_random(
            || {
                assert_eq!(DATA.0, 0xC0FFEE);
                drop(LOCK.lock().unwrap());
                let (tx, rx) = mpsc::channel::<()>();
                *SENDER.lock().unwrap() = Some(tx);
                park_until_teardown(move || {
                    let data: &'static Canary = &DATA;
                    let mut guard = LOCK.lock().unwrap();
                    // Returns once the static sender is dropped.
                    let _ = rx.recv();
                    assert_eq!(data.0, 0xC0FFEE);
                    guard.push(4);
                });
            },
            ITERATIONS,
        )
    });
    assert!(message.contains("deadlock while dropping"), "{message}");
}

/// A destructor that yields while it holds a lock gets to finish before teardown unwinds a stack whose
/// destructors need the lock: those can't wait.
#[test]
fn destructor_that_yields_holding_a_lock_finishes_before_stacks_are_unwound() {
    check_destructors_complete(2, |counter| {
        let mutex = Arc::new(Mutex::new(0));
        let (m, c) = (mutex.clone(), counter.clone());
        park_until_teardown(move || {
            let mut guard = m.lock().unwrap();
            for _ in 0..500 {
                thread::yield_now();
            }
            *guard += 1;
            count(&c);
        });
        park(Wait::Spinning, move || {
            *mutex.lock().unwrap() += 1;
            count(&counter);
        });
    });
}

/// A sender that teardown unwinds while it is first in line for a full channel passes on the turn
/// that a receive gave it.
#[test]
fn channel_wakes_the_next_sender_when_teardown_unwinds_the_first() {
    check_destructors_complete(1, |counter| {
        let (tx, rx) = mpsc::sync_channel::<u32>(1);
        tx.send(1).unwrap();
        let first = tx.clone();
        let (started_tx, started_rx) = mpsc::channel();
        drop(future::spawn_local(async move {
            started_tx.send(()).unwrap();
            // Blocks in the middle of `poll`: the channel is full.
            let _ = first.send(2);
        }));
        started_rx.recv().unwrap();
        park_until_teardown(move || {
            tx.send(3).unwrap();
            count(&counter);
        });
        park_until_teardown(move || {
            assert_eq!(rx.recv(), Ok(1));
            // The receiver stays, so that the senders don't fail.
            std::mem::forget(rx);
        });
    });
}

/// Yields when dropped, and counts the drops as `task`.
struct YieldsOnDrop(TaskId, Arc<StdAtomicUsize>);

impl Drop for YieldsOnDrop {
    fn drop(&mut self) {
        thread::yield_now();
        assert_eq!(current::me(), self.0);
        count(&self.1);
    }
}

/// The payload of a panic that teardown ignores is dropped as the task that panicked, which can use
/// Shuttle then, but doesn't switch out.
#[test]
fn ignored_panic_payload_is_dropped_as_its_task() {
    let dropped = Arc::new(StdAtomicUsize::new(0));
    let counter = dropped.clone();
    let message = panic_message(|| {
        check_random(
            move || {
                park_until_teardown(|| panic!("first destructor panic"));
                let counter = counter.clone();
                park_until_teardown(move || panic::panic_any(YieldsOnDrop(current::me(), counter)));
            },
            1,
        )
    });
    assert_eq!(message, "first destructor panic");
    assert_eq!(dropped.load(StdOrdering::SeqCst), 1);
}

/// The same for a panic out of a stack that teardown unwinds, here of a task that caught the unwind.
#[test]
fn ignored_panic_payload_from_an_unwound_stack_is_dropped_as_its_task() {
    let dropped = Arc::new(StdAtomicUsize::new(0));
    let counter = dropped.clone();
    let message = panic_message(|| {
        check_random(
            move || {
                park_until_teardown(|| panic!("first destructor panic"));
                let counter = counter.clone();
                let (started_tx, started_rx) = mpsc::channel();
                drop(future::spawn_local(async move {
                    let me = current::me();
                    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                        started_tx.send(()).unwrap();
                        // In the middle of `poll` when the execution ends.
                        let spins = AtomicUsize::new(0);
                        loop {
                            spins.fetch_add(1, Ordering::SeqCst);
                        }
                    }));
                    panic::panic_any(YieldsOnDrop(me, counter));
                }));
                started_rx.recv().unwrap();
            },
            1,
        )
    });
    assert_eq!(message, "first destructor panic");
    assert_eq!(dropped.load(StdOrdering::SeqCst), 1);
}

/// A destructor that exceeds the step bound abandons the rest of the execution, as if it had failed:
/// the stacks that are left are leaked, along with a task's default `tracing` dispatcher.
#[test]
fn exceeding_the_step_bound_abandons_the_execution() {
    /// A subscriber that is interested in everything, so that a task's default is parked.
    struct Marker;

    impl tracing::Subscriber for Marker {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, _: &tracing::Event<'_>) {}
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    let mut config = Config::new();
    config.max_steps = MaxSteps::FailAfter(1000);
    let message = panic_message(|| {
        Runner::new(RandomScheduler::new(1), config).run(|| {
            let flag = AtomicBool::new(false);
            park_until_teardown(move || while !flag.load(Ordering::SeqCst) {});
            spawn_until_teardown(Wait::Parked, |started| async move {
                let _default = tracing::subscriber::set_default(Marker);
                started.now();
                Wait::Parked.until_teardown().await;
            });
        })
    });
    assert!(message.contains("exceeded the step bound (1000)"), "{message}");
    assert!(!tracing::dispatcher::get_default(|dispatch| dispatch.is::<Marker>()));
}

/// A `tracing` layer that looks up the current task when a task's span closes finds the task.
#[test]
fn layer_sees_the_task_whose_span_closes() {
    use tracing::span;
    use tracing::Subscriber;
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
    use tracing_subscriber::registry::LookupSpan;

    struct NamesTaskOnClose;

    impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for NamesTaskOnClose {
        fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
            if ctx.span(&id).is_some_and(|span| span.name() == "step") {
                let _ = current::me();
            }
        }
    }

    let subscriber = tracing_subscriber::registry().with(NamesTaskOnClose);
    tracing::subscriber::with_default(subscriber, || {
        check_random(
            || {
                thread::spawn(|| {}).join().unwrap();
                drop(future::spawn(async {}));
            },
            ITERATIONS,
        );
    });
}

/// A stopped execution is abandoned: what destructors cannot do, because the stacks of the
/// execution's tasks are leaked with what they hold, doesn't fail the test.
#[test]
fn stopped_execution_ignores_destructors_that_fail() {
    struct LocksOnDrop(Arc<Mutex<u32>>);

    impl Drop for LocksOnDrop {
        fn drop(&mut self) {
            *self.0.lock().unwrap() += 1;
        }
    }

    shuttle::thread_local! {
        static LOCAL: std::cell::RefCell<Option<LocksOnDrop>> = const { std::cell::RefCell::new(None) };
    }

    let mut config = Config::new();
    config.max_steps = MaxSteps::ContinueAfter(20);
    Runner::new(RandomScheduler::new(ITERATIONS), config).run(|| {
        let mutex = Arc::new(Mutex::new(0));
        LOCAL.with(|local| *local.borrow_mut() = Some(LocksOnDrop(mutex.clone())));
        let _guard = mutex.lock().unwrap();
        let flag = AtomicBool::new(false);
        while !flag.load(Ordering::SeqCst) {}
    });
}

/// A stopped execution drops the functions of the scoped threads that never ran before it frees the
/// stack that they borrow from.
#[test]
fn stopped_execution_drops_scoped_threads_before_their_parents_stack() {
    struct ReadsOnDrop<'a>(&'a StdAtomicUsize, Arc<StdAtomicUsize>);

    impl Drop for ReadsOnDrop<'_> {
        fn drop(&mut self) {
            if self.0.load(StdOrdering::SeqCst) != 0xC0FFEE {
                self.1.fetch_add(1, StdOrdering::SeqCst);
            }
        }
    }

    let bad_reads = Arc::new(StdAtomicUsize::new(0));
    let reads = bad_reads.clone();
    let mut config = Config::new();
    config.max_steps = MaxSteps::ContinueAfter(3);
    Runner::new(RandomScheduler::new(20), config).run(move || {
        let canary = StdAtomicUsize::new(0xC0FFEE);
        thread::scope(|s| {
            for _ in 0..4 {
                let reads_on_drop = ReadsOnDrop(&canary, reads.clone());
                s.spawn(move || {
                    let _reads_on_drop = reads_on_drop;
                    thread::yield_now();
                });
            }
        });
    });
    assert_eq!(bad_reads.load(StdOrdering::SeqCst), 0);
}

/// A panic out of the executor, here from a scheduler, still tears the execution down, and is what
/// fails the test.
#[test]
fn scheduler_panic_tears_down_the_execution() {
    static THREAD_RAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Panics on the decision after a thread finishes, which is made on the executor's stack.
    struct PanicsWhenThreadFinishes(RandomScheduler);

    impl shuttle::scheduler::Scheduler for PanicsWhenThreadFinishes {
        fn new_execution(&mut self) -> Option<shuttle::scheduler::Schedule> {
            self.0.new_execution()
        }

        fn next_task(
            &mut self,
            runnable: &[&shuttle::scheduler::Task],
            current: Option<shuttle::scheduler::TaskId>,
            is_yielding: bool,
        ) -> Option<shuttle::scheduler::TaskId> {
            let current_finished = current.is_some_and(|current| runnable.iter().all(|task| task.id() != current));
            if THREAD_RAN.load(StdOrdering::SeqCst) && current_finished {
                panic!("scheduler failed");
            }
            self.0.next_task(runnable, current, is_yielding)
        }

        fn next_u64(&mut self) -> u64 {
            self.0.next_u64()
        }
    }

    shuttle::lazy_static! {
        static ref STATIC: std::sync::Mutex<Option<UsesShuttleOnDrop>> = std::sync::Mutex::new(None);
    }

    let completed = Arc::new(StdAtomicUsize::new(0));
    let counter = completed.clone();
    let message = panic_message(move || {
        Runner::new(PanicsWhenThreadFinishes(RandomScheduler::new(1)), Config::new()).run(move || {
            *STATIC.lock().unwrap() = Some(UsesShuttleOnDrop(counter.clone()));
            thread::spawn(|| THREAD_RAN.store(true, StdOrdering::SeqCst))
                .join()
                .unwrap();
        })
    });
    assert_eq!(message, "scheduler failed");
    assert_eq!(completed.load(StdOrdering::SeqCst), 1);
}

/// Teardown doesn't extend the schedule, even when destructors draw random numbers, so a failure's
/// schedule is persisted once, and replays.
#[test]
fn random_numbers_in_destructors_do_not_extend_the_schedule() {
    use shuttle::rand::{thread_rng, Rng};

    struct DrawsOnDrop;

    impl Drop for DrawsOnDrop {
        fn drop(&mut self) {
            let _: u64 = thread_rng().gen();
        }
    }

    shuttle::lazy_static! {
        static ref STATIC: std::sync::Mutex<Option<DrawsOnDrop>> = std::sync::Mutex::new(None);
    }

    fn deadlocks() {
        *STATIC.lock().unwrap() = Some(DrawsOnDrop);
        let (tx, rx) = mpsc::channel::<()>();
        let _tx = tx;
        let _ = rx.recv();
    }

    let directory = tempfile::tempdir().unwrap();
    let mut config = Config::new();
    config.failure_persistence = FailurePersistence::File(Some(directory.path().to_path_buf()));
    config.ungraceful_shutdown_config.continuation_function_behavior = ContinuationFunctionBehavior::Drop;
    let message = panic_message(|| Runner::new(RandomScheduler::new(1), config).run(deadlocks));
    assert!(message.contains("deadlock"), "{message}");

    let schedules = std::fs::read_dir(directory.path()).unwrap().collect::<Vec<_>>();
    assert_eq!(schedules.len(), 1, "one failure, one schedule");
    let schedule = schedules.into_iter().next().unwrap().unwrap().path();
    let message = panic_message(|| shuttle::replay_from_file(deadlocks, schedule));
    assert!(message.contains("deadlock"), "{message}");
}

/// A tracing layer that calls into Shuttle as the spans of tasks close finds the execution there.
#[test]
fn task_spans_close_while_the_execution_is_there() {
    use tracing::span;
    use tracing::Subscriber;
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
    use tracing_subscriber::registry::LookupSpan;

    /// Counts the closes of the tasks' `step` spans, and those at which there is no execution any
    /// more.
    struct UsesShuttleOnClose(Arc<StdAtomicUsize>, Arc<StdAtomicUsize>);

    impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for UsesShuttleOnClose {
        fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
            if ctx.span(&id).is_some_and(|span| span.name() == "step") {
                self.0.fetch_add(1, StdOrdering::SeqCst);
                if let Err(payload) = panic::catch_unwind(current::context_switches) {
                    let message = payload.downcast_ref::<&str>().copied().unwrap_or_default();
                    if message.contains("is not set") {
                        self.1.fetch_add(1, StdOrdering::SeqCst);
                    }
                }
            }
        }
    }

    let closes = Arc::new(StdAtomicUsize::new(0));
    let closes_outside = Arc::new(StdAtomicUsize::new(0));
    let subscriber = tracing_subscriber::registry().with(UsesShuttleOnClose(closes.clone(), closes_outside.clone()));
    tracing::subscriber::with_default(subscriber, || {
        check_random(
            || {
                thread::spawn(|| {}).join().unwrap();
                drop(future::spawn(async {}));
                park_until_teardown(|| {});
            },
            1,
        );
    });
    assert!(closes.load(StdOrdering::SeqCst) > 0);
    assert_eq!(closes_outside.load(StdOrdering::SeqCst), 0);
}

/// A destructor that waits by yielding for a destructor on a stack that teardown unwinds doesn't
/// wait long: not as long as a small step bound allows, here.
#[test]
fn destructor_that_waits_by_yielding_for_a_stack_to_be_unwound() {
    let mut config = Config::new();
    config.max_steps = MaxSteps::FailAfter(80);
    check_destructors_complete_with(config, ITERATIONS, 2, |counter| {
        let unwound = Arc::new(StdAtomicBool::new(false));
        let (u, c) = (unwound.clone(), counter.clone());
        park_until_teardown(move || {
            while !u.load(StdOrdering::SeqCst) {
                thread::yield_now();
            }
            count(&c);
        });
        // Blocked in the middle of `poll`, so that teardown unwinds its stack.
        let (started_tx, started_rx) = mpsc::channel();
        drop(future::spawn_local(async move {
            let _on_drop = on_drop(move || {
                unwound.store(true, StdOrdering::SeqCst);
                count(&counter);
            });
            let (tx, rx) = mpsc::channel::<()>();
            started_tx.send(()).unwrap();
            let _tx = tx;
            let _ = rx.recv();
        }));
        started_rx.recv().unwrap();
    });
}

/// A tag's destructor runs as its task, also when the last reference to the tag is the one that
/// `current::set_tag_for_current_task` keeps.
#[allow(deprecated)]
#[test]
fn tag_destructor_runs_as_its_task() {
    #[derive(Debug)]
    struct ChecksTaskOnDrop(TaskId, Arc<StdAtomicUsize>);

    impl current::Taggable for ChecksTaskOnDrop {}

    impl Drop for ChecksTaskOnDrop {
        fn drop(&mut self) {
            assert_eq!(current::me(), self.0);
            count(&self.1);
        }
    }

    check_destructors_complete(1, |counter| {
        thread::spawn(move || {
            current::set_tag_for_current_task(Arc::new(ChecksTaskOnDrop(current::me(), counter)));
        })
        .join()
        .unwrap();
    });
}

/// A task that a destructor spawns while teardown drops what the tasks left behind, here a tag, is
/// torn down too.
#[allow(deprecated)]
#[test]
fn task_spawned_by_a_tag_destructor_is_torn_down() {
    #[derive(Debug)]
    struct SpawnsOnDrop(Arc<StdAtomicUsize>);

    impl current::Taggable for SpawnsOnDrop {}

    impl Drop for SpawnsOnDrop {
        fn drop(&mut self) {
            let parent = current::me();
            let counter = self.0.clone();
            let on_drop = on_drop(move || {
                assert_ne!(current::me(), parent);
                count(&counter);
            });
            drop(future::spawn(async move {
                let _on_drop = on_drop;
            }));
        }
    }

    check_destructors_complete(1, |counter| {
        current::set_tag_for_current_task(Arc::new(SpawnsOnDrop(counter)));
    });
}

/// The tasks' labels are dropped in the order the tasks were created, so that their destructors run
/// in the same order every time.
#[test]
fn labels_are_dropped_in_the_order_of_their_tasks() {
    #[derive(Clone, Debug)]
    struct LogsDrop(usize, Arc<std::sync::Mutex<Vec<usize>>>);

    impl Drop for LogsDrop {
        fn drop(&mut self) {
            self.1.lock().unwrap().push(self.0);
        }
    }

    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let l = log.clone();
    check_random(
        move || {
            let tasks = (1..=8).map(|i| {
                let l = l.clone();
                thread::spawn(move || {
                    current::set_label_for_task(current::me(), LogsDrop(i, l));
                })
            });
            for task in tasks.collect::<Vec<_>>() {
                task.join().unwrap();
            }
            l.lock().unwrap().clear();
        },
        1,
    );
    assert_eq!(*log.lock().unwrap(), (1..=8).collect::<Vec<_>>());
}
