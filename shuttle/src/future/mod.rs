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
use std::panic::Location;
use std::pin::Pin;
use std::result::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

pub mod batch_semaphore;

fn spawn_inner<F>(fut: F, caller: &'static Location<'static>) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    let stack_size = ExecutionState::with(|s| s.config.stack_size);
    let inner = Arc::new(std::sync::Mutex::new(JoinHandleInner::default()));
    let aborted = Arc::new(AtomicBool::new(false));
    let task_id = ExecutionState::spawn_future(
        Wrapper::new(fut, inner.clone(), aborted.clone()),
        stack_size,
        None,
        caller,
    );

    JoinHandle {
        task_id,
        inner,
        aborted,
    }
}

/// Spawn a new async task that the executor will run to completion.
#[track_caller]
pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    spawn_inner(fut, Location::caller())
}

/// Spawn a new async task that the executor will run to completion.
/// This is just `spawn` without the `Send` bound, and it mirrors `spawn_local` from Tokio.
#[track_caller]
pub fn spawn_local<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    spawn_inner(fut, Location::caller())
}

/// An owned permission to abort a spawned task, without awaiting its completion.
#[derive(Debug, Clone)]
pub struct AbortHandle {
    task_id: TaskId,
    aborted: Arc<AtomicBool>,
}

impl AbortHandle {
    /// Abort the task associated with the handle.
    ///
    /// The task will be cancelled at the next await point. If the task has already
    /// completed, this is a no-op.
    pub fn abort(&self) {
        // Scheduling point: the scheduler may run other tasks (including the target)
        // before the abort flag is set, creating interleavings where the task completes
        // normally despite an abort() call.
        thread::switch();

        // Signal the Wrapper to skip the inner future on the next poll.
        // If already aborted, skip the redundant wake.
        if self.aborted.swap(true, Ordering::Relaxed) {
            return;
        }
        // Wake the task so Wrapper::poll() runs and performs cleanup.
        let res = ExecutionState::try_with(|state| {
            if !state.is_finished() {
                state.get_mut(self.task_id).abort();
            }
        });
        if let Err(e) = res {
            tracing::error!("`AbortHandle::abort` failed with error: {e:?}");
        }
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

unsafe impl Send for AbortHandle {}
unsafe impl Sync for AbortHandle {}

/// An owned permission to join on an async task (await its termination).
#[derive(Debug)]
pub struct JoinHandle<T> {
    task_id: TaskId,
    inner: Arc<std::sync::Mutex<JoinHandleInner<T>>>,
    aborted: Arc<AtomicBool>,
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
    ///
    /// The task will be cancelled at the next await point. Awaiting the `JoinHandle` after
    /// calling `abort` will return `Err(JoinError::Cancelled)`, unless the task completed
    /// before the abort took effect.
    pub fn abort(&self) {
        // Scheduling point: the scheduler may run other tasks (including the target)
        // before the abort flag is set, creating interleavings where the task completes
        // normally despite an abort() call.
        thread::switch();

        // Signal the Wrapper to skip the inner future on the next poll.
        // If already aborted, skip the redundant wake.
        if self.aborted.swap(true, Ordering::Relaxed) {
            return;
        }
        // Wake the task so Wrapper::poll() runs and performs cleanup.
        let res = ExecutionState::try_with(|state| {
            if !state.is_finished() {
                state.get_mut(self.task_id).abort();
            }
        });
        if let Err(e) = res {
            tracing::error!("`JoinHandle::abort` failed with error: {e:?}");
        }
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

    /// Returns a new `AbortHandle` that can be used to remotely abort this task.
    pub fn abort_handle(&self) -> AbortHandle {
        AbortHandle {
            task_id: self.task_id,
            aborted: self.aborted.clone(),
        }
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
        // Detach the task so it keeps running but we don't wait for it.
        // Dropping a JoinHandle does NOT cancel the task (unlike abort()).
        let res = ExecutionState::try_with(|state| {
            if !state.is_finished() {
                state.get_mut(self.task_id).detach();
            }
        });
        if let Err(e) = res {
            tracing::error!("`JoinHandle::drop` failed with error: {e:?}");
        }
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
//
// The `aborted` flag is set by `JoinHandle::abort()` / `AbortHandle::abort()`. On the
// next poll, `Wrapper` detects the flag, drops the inner future (running its destructors),
// cleans up thread-local storage, publishes `Err(JoinError::Cancelled)` to any awaiting
// join handle, and returns `Poll::Ready(())`.
struct Wrapper<F: Future> {
    /// The inner future. Wrapped in `Option` so we can drop it explicitly in the
    /// abort path (before running thread-local destructors).
    future: Option<Pin<Box<F>>>,
    inner: Arc<std::sync::Mutex<JoinHandleInner<F::Output>>>,
    aborted: Arc<AtomicBool>,
}

impl<F> Wrapper<F>
where
    F: Future + 'static,
    F::Output: 'static,
{
    fn new(future: F, inner: Arc<std::sync::Mutex<JoinHandleInner<F::Output>>>, aborted: Arc<AtomicBool>) -> Self {
        Self {
            future: Some(Box::pin(future)),
            inner,
            aborted,
        }
    }
}

impl<F> Wrapper<F>
where
    F: Future + 'static,
    F::Output: 'static,
{
    /// Clean up thread-local storage, publish the result to the JoinHandle, and wake
    /// any task waiting on the result.
    fn finish(&self, result: Result<F::Output, JoinError>) {
        // Run thread-local destructors.
        // See `pop_local` for details on why this loop looks slightly funky.
        while let Some(local) = ExecutionState::with(|state| state.current_mut().pop_local()) {
            drop(local);
        }

        let mut lock = self.inner.lock().unwrap();
        lock.result = Some(result);
        if let Some(waker) = lock.waker.take() {
            waker.wake();
        }
    }
}

impl<F> Future for Wrapper<F>
where
    F: Future + 'static,
    F::Output: 'static,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        // If abort() was called, skip polling the inner future.
        if this.aborted.load(Ordering::Relaxed) {
            // If the execution is already finished (e.g., the runtime is shutting down),
            // we can't access task state for cleanup. Just return Ready to let the task
            // be cleaned up by the runtime.
            if ExecutionState::try_with(|state| state.is_finished()).unwrap_or(true) {
                return Poll::Ready(());
            }

            // Drop the inner future first so its destructors can still access TLS.
            this.future.take();
            this.finish(Err(JoinError::Cancelled));
            return Poll::Ready(());
        }

        match this.future.as_mut().unwrap().as_mut().poll(cx) {
            Poll::Ready(result) => {
                // If we've finished execution already (this task was detached), don't clean up. We
                // can't access the state any more to destroy thread locals, and don't want to run
                // any more wakers (which will be no-ops anyway).
                if ExecutionState::try_with(|state| state.is_finished()).unwrap_or(true) {
                    return Poll::Ready(());
                }

                this.finish(Ok(result));
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

    // Note: we only switch on poll pending, since this blocks the current task. This means that *internal*
    // Shuttle futures which do not use other Shuttle primitives such as `batch_semaphore::Acquire` must
    // have a scheduling point prior to first poll if that poll will be successful and can affect other tasks.
    // For example, an uncontested acquire makes other threads block or fail try-acquires, so there must be
    // a scheduling point for scheduling completeness. For *external* futures, this is a non-issue because they
    // should use other Shuttle primitives inside of `poll` if polling can affect other threads.
    loop {
        match future.as_mut().poll(cx) {
            Poll::Ready(result) => break result,
            Poll::Pending => {
                ExecutionState::with(|state| state.current_mut().sleep_unless_woken());
                thread::switch();
            }
        }
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
