//! Tests for destructors that use tokio primitives while an execution is torn down: when it ends,
//! Shuttle drops the tasks that have not finished, which runs their destructors.

use shuttle::check_random;
use shuttle::future;
use shuttle::sync::mpsc;
use shuttle_tokio_impl_inner::sync::{mpsc as tokio_mpsc, oneshot, Mutex, Notify, Semaphore};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
/// task, so a destructor that runs after that can lock it.
#[test]
fn mutex_held_by_task_is_released_during_teardown() {
    let locked = Arc::new(AtomicBool::new(false));
    let flag = locked.clone();
    check_random(
        move || {
            let mutex = Arc::new(Mutex::new(0));
            let (held_tx, held_rx) = mpsc::channel();
            let m = mutex.clone();
            drop(future::spawn(async move {
                let _guard = m.lock().await;
                held_tx.send(()).unwrap();
                futures::future::pending::<()>().await;
            }));
            held_rx.recv().unwrap();
            let flag = flag.clone();
            park_until_teardown(move || {
                *mutex.blocking_lock() += 1;
                flag.store(true, Ordering::SeqCst);
            });
        },
        ITERATIONS,
    );
    assert!(locked.load(Ordering::SeqCst));
}
