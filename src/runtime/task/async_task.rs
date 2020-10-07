use crate::runtime::task::*;
use futures::future::BoxFuture;
use std::task::{Context, Poll};

pub(crate) struct AsyncTask {
    future: BoxFuture<'static, ()>,
}

pub(crate) fn new(future: BoxFuture<'static, ()>) -> AsyncTask {
    AsyncTask { future }
}

impl AtomicTask for AsyncTask {
    fn step(&mut self) -> TaskResult {
        // TODO Using a fake waker/context here, since we don't really need a waker, do we?
        let waker = futures::task::noop_waker_ref();
        let mut cx = &mut Context::from_waker(waker);
        let ret = self.future.as_mut().poll(&mut cx);
        match ret {
            Poll::Ready(()) => TaskResult::Finished,
            Poll::Pending => TaskResult::Yielded, // TODO: fix this
        }
    }

    fn cleanup(self: Box<Self>) {}
}
