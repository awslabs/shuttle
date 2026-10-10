//! Tests for destructors that use tokio primitives while an execution is torn down: when it ends,
//! Shuttle drops the tasks that have not finished, which runs their destructors.

use shuttle::check_random;
use shuttle::future;
use shuttle::sync::mpsc;
use shuttle_tokio_impl_inner::sync::{mpsc as tokio_mpsc, oneshot, Mutex, Notify, Semaphore};
use std::sync::atomic::{AtomicUsize, Ordering};
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
/// ends with the task parked. Returns once the task has started.
fn park_until_teardown(on_drop: impl FnOnce() + Send + 'static) {
    let (started_tx, started_rx) = mpsc::channel();
    drop(future::spawn(async move {
        let _on_drop = OnDrop(Some(on_drop));
        started_tx.send(()).unwrap();
        futures::future::pending::<()>().await;
    }));
    started_rx.recv().unwrap();
}

/// A destructor that stops a background job: it takes the job's lock with `try_lock`, and then
/// sets the job's stop flag and wakes the job's tasks.
#[test]
fn destructor_stops_a_job_during_teardown() {
    let stopped = Arc::new(AtomicUsize::new(0));
    let counter = stopped.clone();
    check_random(
        move || {
            let job = Arc::new(Mutex::new(()));
            let stop_flag = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
            let stop = Arc::new(Notify::new());
            let counter = counter.clone();
            park_until_teardown(move || {
                if let Ok(_job) = job.try_lock() {
                    stop_flag.store(true, shuttle::sync::atomic::Ordering::Release);
                    stop.notify_waiters();
                    stop.notify_one();
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            });
        },
        ITERATIONS,
    );
    assert_eq!(stopped.load(Ordering::SeqCst), ITERATIONS);
}

/// Destructors that use tokio's channels, semaphores and tasks during teardown.
#[test]
fn destructors_use_channels_and_tasks_during_teardown() {
    let completed = Arc::new(AtomicUsize::new(0));
    let counter = completed.clone();
    check_random(
        move || {
            let (oneshot_tx, oneshot_rx) = oneshot::channel::<u32>();
            let (mpsc_tx, mpsc_rx) = tokio_mpsc::unbounded_channel::<u32>();
            let semaphore = Arc::new(Semaphore::new(1));
            let counter = counter.clone();
            park_until_teardown(move || {
                let _oneshot_rx = oneshot_rx;
                let _mpsc_rx = mpsc_rx;
                oneshot_tx.send(1).unwrap();
                mpsc_tx.send(1).unwrap();
                drop(semaphore.try_acquire().unwrap());
                drop(shuttle_tokio_impl_inner::spawn(async {}));
                counter.fetch_add(1, Ordering::SeqCst);
            });
        },
        ITERATIONS,
    );
    assert_eq!(completed.load(Ordering::SeqCst), ITERATIONS);
}

/// A tokio mutex that a task holds when the execution ends is released when teardown drops the
/// task, so a destructor can lock it, whichever task teardown drops first: a destructor that has to
/// wait for the lock waits.
#[test]
fn mutex_held_by_task_is_released_during_teardown() {
    for holder_first in [true, false] {
        let locked = Arc::new(AtomicUsize::new(0));
        let counter = locked.clone();
        check_random(
            move || {
                let mutex = Arc::new(Mutex::new(0));
                let m = mutex.clone();
                let counter = counter.clone();
                let lock_on_drop = move || {
                    *m.blocking_lock() += 1;
                    counter.fetch_add(1, Ordering::SeqCst);
                };
                if !holder_first {
                    park_until_teardown(lock_on_drop.clone());
                }
                let (held_tx, held_rx) = mpsc::channel();
                drop(future::spawn(async move {
                    let _guard = mutex.lock().await;
                    held_tx.send(()).unwrap();
                    futures::future::pending::<()>().await;
                }));
                held_rx.recv().unwrap();
                if holder_first {
                    park_until_teardown(lock_on_drop);
                }
            },
            ITERATIONS,
        );
        assert_eq!(locked.load(Ordering::SeqCst), ITERATIONS);
    }
}

/// A destructor that panics during teardown fails the test, but teardown goes on, and the
/// destructors that run later can still lock a tokio mutex whose guard that panic dropped.
#[test]
fn mutex_works_after_a_destructor_panicked_during_teardown() {
    let completed = Arc::new(AtomicUsize::new(0));
    let counter = completed.clone();
    let result = std::panic::catch_unwind(move || {
        check_random(
            move || {
                let mutex = Arc::new(Mutex::new(0));
                let (held_tx, held_rx) = mpsc::channel();
                let m = mutex.clone();
                drop(future::spawn(async move {
                    let guard = m.lock().await;
                    // Dropped in declaration order: the panic first, then the guard as it unwinds.
                    let _pair = (OnDrop(Some(|| panic!("a destructor panicked"))), guard);
                    held_tx.send(()).unwrap();
                    futures::future::pending::<()>().await;
                }));
                held_rx.recv().unwrap();
                let counter = counter.clone();
                park_until_teardown(move || {
                    *mutex.blocking_lock() += 1;
                    counter.fetch_add(1, Ordering::SeqCst);
                });
            },
            1,
        )
    });
    let payload = result.expect_err("the destructor's panic fails the test");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"a destructor panicked"));
    assert_eq!(completed.load(Ordering::SeqCst), 1);
}

/// A destructor that sleeps (which yields, in Shuttle) while it holds a tokio mutex gets to finish
/// before teardown unwinds a stack whose destructor needs the mutex: that one can't wait.
#[test]
fn destructor_that_sleeps_holding_a_mutex_finishes_before_stacks_are_unwound() {
    let completed = Arc::new(AtomicUsize::new(0));
    let counter = completed.clone();
    check_random(
        move || {
            let mutex = Arc::new(Mutex::new(0));
            let (m, c) = (mutex.clone(), counter.clone());
            park_until_teardown(move || {
                future::block_on(async {
                    let mut guard = m.lock().await;
                    shuttle_tokio_impl_inner::time::sleep(std::time::Duration::from_millis(1)).await;
                    *guard += 1;
                });
                c.fetch_add(1, Ordering::SeqCst);
            });
            // In the middle of `poll`, spinning, so teardown has to unwind it.
            let (started_tx, started_rx) = mpsc::channel();
            let counter = counter.clone();
            drop(future::spawn(async move {
                let _on_drop = OnDrop(Some(move || {
                    *mutex.blocking_lock() += 1;
                    counter.fetch_add(1, Ordering::SeqCst);
                }));
                started_tx.send(()).unwrap();
                let spins = shuttle::sync::atomic::AtomicUsize::new(0);
                loop {
                    spins.fetch_add(1, shuttle::sync::atomic::Ordering::SeqCst);
                }
            }));
            started_rx.recv().unwrap();
        },
        ITERATIONS,
    );
    assert_eq!(completed.load(Ordering::SeqCst), 2 * ITERATIONS);
}

/// A detached task whose `watch::Sender::send_modify` closure panics fails the test, unless the
/// execution ends before the panic gets out of `send_modify`. The sender catches the panic, releases
/// its lock, which is a scheduling point, and then resumes the panic with
/// `std::panic::resume_unwind`, which doesn't call the panic hook. The unwind switches out as it
/// releases a mutex, so the execution could end before the task resumes.
#[test]
fn panic_in_send_modify_of_detached_task_fails_test() {
    let mut failures = 0;
    for seed in 0..50 {
        let result = std::panic::catch_unwind(|| {
            shuttle::check_random_with_seed(
                || {
                    let (tx, rx) = shuttle_tokio_impl_inner::sync::watch::channel(0u32);
                    let mutex = Arc::new(Mutex::new(0));
                    drop(future::spawn(async move {
                        let _rx = rx;
                        let _guard = mutex.lock().await;
                        tx.send_modify(|_| panic!("modify failed"));
                    }));
                    for _ in 0..3 {
                        shuttle::thread::yield_now();
                    }
                },
                seed,
                1,
            )
        });
        if let Err(payload) = result {
            assert_eq!(payload.downcast_ref::<&str>(), Some(&"modify failed"));
            failures += 1;
        }
        assert!(!std::thread::panicking(), "a panic was left unwinding");
    }
    assert!(failures > 0);
}
