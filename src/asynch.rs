//! Shuttle's implementation of an async executor, roughly equivalent to [`futures::executor`].
//!
//! The [spawn] method spawns a new asynchronous task that the executor will run to completion. The
//! [block_on] method blocks the current thread on the completion of a future.
//!
//! [`futures::executor`]: https://docs.rs/futures/0.3.13/futures/executor/index.html

use crate::runtime::execution::ExecutionState;
use crate::runtime::task::TaskId;
use crate::runtime::thread;
use std::future::Future;
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
    let stack_size = ExecutionState::with(|s| s.config.stack_size);
    let task_id = ExecutionState::spawn_future(Wrapper::new(fut, std::sync::Arc::clone(&result)), stack_size, None);

    thread::switch();

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

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        ExecutionState::try_with(|state| {
            if !state.is_finished() {
                let task = state.get_mut(self.task_id);
                task.detach();
            }
        });
    }
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
                // Run thread-local destructors before publishing the result.
                // See `pop_local` for details on why this loop looks this slightly funky way.
                // TODO: thread locals and futures don't mix well right now. each task gets its own
                //       thread local storage, but real async code might know that its executor is
                //       single-threaded and so use TLS to share objects across all tasks.
                while let Some(local) = ExecutionState::with(|state| state.current_mut().pop_local()) {
                    drop(local);
                }

                *self.result.lock().unwrap() = Some(Ok(result));

                // Unblock our waiter if we have one and it's still alive
                ExecutionState::with(|state| {
                    if let Some(waiter) = state.current_mut().take_waiter() {
                        if !state.get_mut(waiter).finished() {
                            state.get_mut(waiter).unblock();
                        }
                    }
                });

                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Run a future to completion on the current thread.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let waker = ExecutionState::with(|state| state.current_mut().waker());
    let cx = &mut Context::from_waker(&waker);

    thread::switch();

    loop {
        match future.as_mut().poll(cx) {
            Poll::Ready(result) => break result,
            Poll::Pending => {
                ExecutionState::with(|state| {
                    state.current_mut().block_unless_self_woken();
                });
            }
        }

        thread::switch();
    }
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
