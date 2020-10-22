//! Shuttle's implementation of `async_runtime::spawn`.

use crate::runtime::execution::Execution;
use crate::runtime::task::TaskId;
use crate::runtime::thread_future;
use futures::future::Future;
use std::pin::Pin;
use std::result::Result;
use std::task::{Context, Poll};

/// Spawn a new async task.
pub fn spawn<T, F>(fut: F) -> JoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let result = std::sync::Arc::new(std::sync::Mutex::new(None));
    let task_id = Execution::spawn(Wrapper::new(fut, std::sync::Arc::clone(&result)));
    JoinHandle { task_id, result }
}

/// An owned permission to join on an async task (await its termination).
#[derive(Debug)]
pub struct JoinHandle<T> {
    task_id: TaskId,
    result: std::sync::Arc<std::sync::Mutex<Option<Result<T, JoinError>>>>,
}

impl<T> JoinHandle<T> {
    // TODO: providing this because tokio does
    /// Abort the task associated with the handle.
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
        // Check if result is available
        let result = self.result.lock().unwrap().take();
        if let Some(result) = result {
            Poll::Ready(Ok(result.expect("task should have finished")))
        } else {
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

                Execution::with_state(|state| {
                    if let Some(waiter) = state.current().waiter() {
                        let waiter = state.get_mut(waiter);
                        assert!(waiter.blocked());
                        waiter.unblock();
                    }
                });

                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Block a thread on a future
pub fn block_on<T, F>(future: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let handle = spawn(future);

    thread_future::switch(); // Required to allow Execution to spawn the future

    Execution::with_state(|state| {
        let me = state.current().id();
        let target = state.get_mut(handle.task_id);
        if target.wait_for(me) {
            state.current_mut().block();
        }
    });

    thread_future::switch(); // Required in case the thread blocked

    let result = handle.result.lock().unwrap().take();
    result.unwrap().expect("result should be available to waiter")
}
