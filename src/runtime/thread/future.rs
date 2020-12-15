use crate::runtime::thread::continuation::{ContinuationPool, PooledContinuation};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A `ThreadFuture` is a Future that wraps around a regular thread, implemented as a continuation.
///
/// Polling the `ThreadFuture` resumes executing the thread until it decides to yield. If it
/// yields because the thread has terminated, the poll returns Ready, otherwise it returns Pending.
///
/// The `ThreadFuture` is a compatibility layer that allows us to implement standard threading
/// constructs (`std::thread` and `std::sync`) on top of Shuttle's futures-based executor.
pub(crate) struct ThreadFuture {
    continuation: PooledContinuation,
}

impl ThreadFuture {
    pub(crate) fn new<F>(f: F) -> ThreadFuture
    where
        F: FnOnce() + Send + 'static,
    {
        let mut continuation = ContinuationPool::acquire();
        continuation.initialize(Box::new(f));
        ThreadFuture { continuation }
    }
}

impl Future for ThreadFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.continuation.resume() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}
