//! Shuttle's implementation of `std::thread`.

use crate::runtime::execution::Execution;
use crate::runtime::task::TaskId;
use crate::runtime::thread_future;
use crate::runtime::thread_future::ThreadFuture;

/// Spawn a new thread, returning a JoinHandle for it.
///
/// The join handle can be used (via the `join` method) to block until the child thread has
/// finished.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    let result = std::sync::Arc::new(std::sync::Mutex::new(None));
    let task_id = {
        let result = std::sync::Arc::clone(&result);
        // Build a new ThreadFuture that will simulate a thread inside a Future
        let task = ThreadFuture::new(move || {
            let ret = f();
            // Publish the result and unblock the waiter. We need to do this now, because once this
            // closure completes, the Execution will consider this task Finished and invoke the
            // scheduler.
            *result.lock().unwrap() = Some(Ok(ret));
            Execution::with_state(|state| {
                if let Some(waiter) = state.current().waiter() {
                    let waiter = state.get_mut(waiter);
                    assert!(waiter.blocked());
                    waiter.unblock();
                }
            });
        });
        Execution::spawn(task)
    };

    thread_future::switch();

    JoinHandle { task_id, result }
}

/// An owned permission to join on a thread (block on its termination).
#[derive(Debug)]
pub struct JoinHandle<T> {
    task_id: TaskId,
    result: std::sync::Arc<std::sync::Mutex<Option<std::thread::Result<T>>>>,
}

impl<T> JoinHandle<T> {
    /// Waits for the associated thread to finish.
    pub fn join(self) -> std::thread::Result<T> {
        Execution::with_state(|state| {
            let me = state.current().id();
            let target = state.get_mut(self.task_id);
            if target.wait_for(me) {
                state.current_mut().block();
            }
        });

        // TODO can we soundly skip the yield if the target thread has already finished?
        thread_future::switch();

        self.result.lock().unwrap().take().expect("target should have finished")
    }
}

// TODO: don't need this? Just call switch directly?
/// Cooperatively gives up a timeslice to the Shuttle scheduler.
pub fn yield_now() {
    thread_future::switch();
}
