//! Shuttle's implementation of an async executor, roughly equivalent to [`futures::executor`].
//!
//! The [spawn] method spawns a new asynchronous task that the executor will run to completion. The
//! [block_on] method blocks the current thread on the completion of a future.
//!
//! [`futures::executor`]: https://docs.rs/futures/0.3.13/futures/executor/index.html

use crate::runtime::execution::ExecutionState;
use crate::runtime::task::{TaskId, TaskType};
use crate::runtime::thread;
use futures::future::Future;
use std::pin::Pin;
use std::result::Result;
use std::task::{Context, Poll};

/// Spawn a new async task that the executor will run to completion.
pub fn spawn<T, F>(fut: F) -> JoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let result = std::sync::Arc::new(std::sync::Mutex::new(None));
    let task_id = ExecutionState::spawn(
        Wrapper::new(fut, std::sync::Arc::clone(&result)),
        TaskType::Future,
        None,
    );
    // TODO I think we need to yield here to give the spawned task a chance to execute before the spawner continues
    JoinHandle { task_id, result }
}

/// An owned permission to join on an async task (await its termination).
#[derive(Debug)]
pub struct JoinHandle<T> {
    task_id: TaskId,
    result: std::sync::Arc<std::sync::Mutex<Option<Result<T, JoinError>>>>,
}

impl<T> JoinHandle<T> {
    /// Abort the task associated with the handle.
    // TODO implement (only tokio provides this)
    pub fn abort(&self) {
        unimplemented!();
    }
}

// TODO: need to work out all the error cases here
/// Task failed to execute to completion.
#[derive(Debug)]
pub enum JoinError {
    /// Task was aborted
    Cancelled,
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(result) = self.result.lock().unwrap().take() {
            Poll::Ready(result)
        } else {
            ExecutionState::with(|state| {
                let me = state.current().id();
                let r = state.get_mut(self.task_id).set_waiter(me);
                assert!(r, "task shouldn't be finished if no result is present");
            });
            Poll::Pending
        }
    }
}

// We wrap a task returning a value inside a wrapper task that returns ().  The wrapper
// contains a mutex-wrapped field that stores the value where it can be passed to a task
// waiting on the join handle.
struct Wrapper<T, F> {
    future: Pin<Box<F>>,
    result: std::sync::Arc<std::sync::Mutex<Option<Result<T, JoinError>>>>,
}

impl<T, F> Wrapper<T, F>
where
    F: Future<Output = T> + Send + 'static,
{
    fn new(future: F, result: std::sync::Arc<std::sync::Mutex<Option<Result<T, JoinError>>>>) -> Self {
        Self {
            future: Box::pin(future),
            result,
        }
    }
}

impl<T, F> Future for Wrapper<T, F>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.future.as_mut().poll(cx) {
            Poll::Ready(result) => {
                *self.result.lock().unwrap() = Some(Ok(result));

                // Unblock our waiter if we have one
                ExecutionState::with(|state| {
                    if let Some(waiter) = state.current_mut().take_waiter() {
                        state.get_mut(waiter).unblock();
                    }
                });

                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Run a future to completion on the current thread.
pub fn block_on<T, F>(future: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let handle = spawn(future);

    thread::switch(); // Required to allow Execution to spawn the future

    ExecutionState::with(|state| {
        let me = state.current().id();
        let target = state.get_mut(handle.task_id);
        if target.set_waiter(me) {
            state.current_mut().block();
        }
    });

    thread::switch(); // Required in case the thread blocked

    let result = handle.result.lock().unwrap().take();
    result
        .expect("result should be available to waiter")
        .expect("task should not fail")
}

/// Yields execution back to the scheduler.
///
/// Borrowed from the Tokio implementation.
#[must_use = "yield_now does nothing unless polled/`await`-ed"]
pub async fn yield_now() {
    /// Yield implementation
    struct YieldNow {
        yielded: bool,
    }

    impl Future for YieldNow {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.yielded {
                return Poll::Ready(());
            }

            self.yielded = true;
            cx.waker().wake_by_ref();
            ExecutionState::request_yield();
            Poll::Pending
        }
    }

    YieldNow { yielded: false }.await
}
