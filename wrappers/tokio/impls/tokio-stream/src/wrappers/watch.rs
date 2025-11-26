use std::pin::Pin;
use tokio::sync::watch::Receiver;

use futures_core::Stream;
use tokio_util::sync::ReusableBoxFuture;

use std::fmt;
use std::task::{Context, Poll};
use tokio::sync::watch::error::RecvError;

/// A wrapper around [`tokio::sync::watch::Receiver`] that implements [`Stream`].
///
/// This stream will start by yielding the current value when the `WatchStream` is polled,
/// regardless of whether it was the initial value or sent afterwards,
/// unless you use [`WatchStream<T>::from_changes`].
///
/// [`tokio::sync::watch::Receiver`]: struct@tokio::sync::watch::Receiver
/// [`Stream`]: trait@crate::Stream
#[cfg_attr(docsrs, doc(cfg(feature = "sync")))]
pub struct WatchStream<T> {
    inner: ReusableBoxFuture<'static, (Result<(), RecvError>, Receiver<T>)>,
}

async fn make_future<T: Clone + Send + Sync>(
    mut rx: Receiver<T>,
) -> (Result<(), RecvError>, Receiver<T>) {
    let result = rx.changed().await;
    (result, rx)
}

impl<T: 'static + Clone + Send + Sync> WatchStream<T> {
    /// Create a new `WatchStream`.
    pub fn new(rx: Receiver<T>) -> Self {
        Self {
            inner: ReusableBoxFuture::new(async move { (Ok(()), rx) }),
        }
    }

    /// Create a new `WatchStream` that waits for the value to be changed.
    pub fn from_changes(rx: Receiver<T>) -> Self {
        Self {
            inner: ReusableBoxFuture::new(make_future(rx)),
        }
    }
}

impl<T: Clone + 'static + Send + Sync> Stream for WatchStream<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let (result, mut rx) = ready!(self.inner.poll(cx));
        if let Ok(()) = result {
            let received = (*rx.borrow_and_update()).clone();
            self.inner.set(make_future(rx));
            Poll::Ready(Some(received))
        } else {
            self.inner.set(make_future(rx));
            Poll::Ready(None)
        }
    }
}

impl<T> Unpin for WatchStream<T> {}

impl<T> fmt::Debug for WatchStream<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WatchStream").finish()
    }
}

impl<T: 'static + Clone + Send + Sync> From<Receiver<T>> for WatchStream<T> {
    fn from(recv: Receiver<T>) -> Self {
        Self::new(recv)
    }
}
