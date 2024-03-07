//! Shuttle's implementation of an async executor, roughly equivalent to [`futures::executor`].
//!
//! The [spawn] method spawns a new asynchronous task that the executor will run to completion. The
//! [block_on] method blocks the current thread on the completion of a future.
//!
//! [`futures::executor`]: https://docs.rs/futures/0.3.13/futures/executor/index.html

use crate::runtime::execution::ExecutionState;
use crate::runtime::task::TaskId;
use crate::runtime::thread;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::result::Result;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

/// Spawn a new async task that the executor will run to completion.
pub fn spawn<T, F>(fut: F) -> JoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let stack_size = ExecutionState::with(|s| s.config.stack_size);
    let inner = Arc::new(std::sync::Mutex::new(JoinHandleInner::default()));
    let task_id = ExecutionState::spawn_future(Wrapper::new(fut, inner.clone()), stack_size, None);

    thread::switch();

    JoinHandle { task_id, inner }
}

/// An owned permission to join on an async task (await its termination).
#[derive(Debug)]
pub struct JoinHandle<T> {
    task_id: TaskId,
    inner: std::sync::Arc<std::sync::Mutex<JoinHandleInner<T>>>,
}

#[derive(Debug)]
struct JoinHandleInner<T> {
    result: Option<Result<T, JoinError>>,
    waker: Option<Waker>,
}

impl<T> Default for JoinHandleInner<T> {
    fn default() -> Self {
        JoinHandleInner {
            result: None,
            waker: None,
        }
    }
}

impl<T> JoinHandle<T> {
    /// Abort the task associated with the handle.
    pub fn abort(&self) {
        ExecutionState::try_with(|state| {
            if !state.is_finished() {
                let task = state.get_mut(self.task_id);
                task.detach();
            }
        });
    }

    /// Returns `true` if this task is finished, otherwise returns `false`.
    ///
    /// ## Panics
    /// Panics if called outside of shuttle context, i.e. if there is no execution context.
    pub fn is_finished(&self) -> bool {
        ExecutionState::with(|state| {
            let task = state.get(self.task_id);
            task.finished()
        })
    }
}

// TODO: need to work out all the error cases here
/// Task failed to execute to completion.
#[derive(Debug)]
pub enum JoinError {
    /// Task was aborted
    Cancelled,
}

impl Display for JoinError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            JoinError::Cancelled => write!(f, "task was cancelled"),
        }
    }
}

impl Error for JoinError {}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        self.abort();
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut lock = self.inner.lock().unwrap();
        if let Some(result) = lock.result.take() {
            Poll::Ready(result)
        } else {
            lock.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

// We wrap a task returning a value inside a wrapper task that returns (). The wrapper
// contains a mutex-wrapped field that stores the value and the waker for the task
// waiting on the join handle. When `poll` returns `Poll::Ready`, the `Wrapper` stores
// the result in the `result` field and wakes the `waker`.
struct Wrapper<T, F> {
    future: Pin<Box<F>>,
    inner: std::sync::Arc<std::sync::Mutex<JoinHandleInner<T>>>,
}

impl<T, F> Wrapper<T, F>
where
    F: Future<Output = T> + Send + 'static,
{
    fn new(future: F, inner: std::sync::Arc<std::sync::Mutex<JoinHandleInner<T>>>) -> Self {
        Self {
            future: Box::pin(future),
            inner,
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
                // If we've finished execution already (this task was detached), don't clean up. We
                // can't access the state any more to destroy thread locals, and don't want to run
                // any more wakers (which will be no-ops anyway).
                if ExecutionState::try_with(|state| state.is_finished()).unwrap_or(true) {
                    return Poll::Ready(());
                }

                // Run thread-local destructors before publishing the result.
                // See `pop_local` for details on why this loop looks this slightly funky way.
                // TODO: thread locals and futures don't mix well right now. each task gets its own
                //       thread local storage, but real async code might know that its executor is
                //       single-threaded and so use TLS to share objects across all tasks.
                while let Some(local) = ExecutionState::with(|state| state.current_mut().pop_local()) {
                    drop(local);
                }

                let mut lock = self.inner.lock().unwrap();
                lock.result = Some(Ok(result));

                if let Some(waker) = lock.waker.take() {
                    waker.wake();
                }

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
                ExecutionState::with(|state| state.current_mut().sleep_unless_woken());
            }
        }

        thread::switch();
    }
}

/// Yields execution back to the scheduler.
///
/// Borrowed from the Tokio implementation.
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
