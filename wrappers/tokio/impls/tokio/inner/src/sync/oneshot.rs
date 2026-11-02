//! A one-shot channel is used for sending a single message between
//! asynchronous tasks. The [`channel`] function is used to create a
//! [`Sender`] and [`Receiver`] handle pair that form the channel.

use futures::channel::oneshot;
use shuttle::future;
use shuttle_engine::runtime::thread;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tracing::trace;

/// Sends a value to the associated [`Receiver`].
#[derive(Debug)]
pub struct Sender<T> {
    // `None` once `send` has taken it
    inner: Option<oneshot::Sender<T>>,
}

/// Receives a value from the associated [`Sender`].
#[derive(Debug)]
pub struct Receiver<T> {
    inner: oneshot::Receiver<T>,
    // Whether the last call was a `poll` that returned `Pending`. The next poll then runs after a
    // wake-up, at the start of a step, so it takes no scheduling point.
    poll_pending: bool,
}

pub mod error {
    //! Oneshot error types.
    use std::fmt;

    /// Error returned by the `Future` implementation for `Receiver`.
    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct RecvError(pub(super) ());

    /// Error returned by the `try_recv` function on `Receiver`.
    #[derive(Debug, Eq, PartialEq, Clone)]
    pub enum TryRecvError {
        /// The send half of the channel has not yet sent a value.
        Empty,

        /// The send half of the channel was dropped without sending a value.
        Closed,
    }

    // ===== impl RecvError =====

    impl fmt::Display for RecvError {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(fmt, "channel closed")
        }
    }

    impl std::error::Error for RecvError {}

    // ===== impl TryRecvError =====

    impl fmt::Display for TryRecvError {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                TryRecvError::Empty => write!(fmt, "channel empty"),
                TryRecvError::Closed => write!(fmt, "channel closed"),
            }
        }
    }

    impl std::error::Error for TryRecvError {}
}
use self::error::{RecvError, TryRecvError};

/// Creates a new one-shot channel for sending single values across asynchronous
/// tasks.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = oneshot::channel();
    (
        Sender { inner: Some(tx) },
        Receiver {
            inner: rx,
            poll_pending: false,
        },
    )
}

impl<T> Sender<T> {
    fn inner(&mut self) -> &mut oneshot::Sender<T> {
        self.inner
            .as_mut()
            .expect("only `send` takes the inner sender, and it consumes `self`")
    }

    /// Attempts to send a value on this channel, returning it back if it could
    /// not be sent.
    pub fn send(mut self, t: T) -> Result<(), T> {
        let inner = self
            .inner
            .take()
            .expect("only `send` takes the inner sender, and it consumes `self`");
        trace!("Sending message on oneshot {:p}", &inner);
        // Scheduling point precedes the visible send operation
        shuttle::thread::yield_now();
        inner.send(t)
    }

    /// Waits for the associated [`Receiver`] handle to close.
    pub async fn closed(&mut self) {
        trace!("sender waiting for oneshot {:p} to close", self.inner());
        // Scheduling point precedes the first read of the channel's state, at the first poll of
        // this future
        thread::switch();
        self.inner().cancellation().await;
    }

    /// Returns `true` if the associated [`Receiver`] handle has been dropped.
    pub fn is_closed(&self) -> bool {
        self.inner
            .as_ref()
            .expect("only `send` takes the inner sender, and it consumes `self`")
            .is_canceled()
    }

    /// Checks whether the oneshot channel has been closed, and if not, schedules the
    /// `Waker` in the provided `Context` to receive a notification when the channel is
    /// closed.
    ///
    /// Note that on multiple calls to poll, only the `Waker` from the `Context` passed
    /// to the most recent call will be scheduled to receive a wakeup.
    pub fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        // Scheduling point precedes the read of the channel's state, on every call
        thread::switch();
        self.inner().poll_canceled(cx)
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        // Scheduling point precedes the visible close of the channel by a `Sender` that didn't send
        if let Some(inner) = &self.inner {
            trace!("dropping oneshot sender {:p} without sending", inner);
            thread::switch_if_not_stopping();
        }
    }
}

impl<T> Receiver<T> {
    /// Prevents the associated [`Sender`] handle from sending a value.
    pub fn close(&mut self) {
        // Scheduling point precedes the visible close operation
        shuttle::thread::yield_now();
        self.poll_pending = false;
        self.inner.close();
    }

    /// Attempts to receive a value.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        // Scheduling point precedes the visible receive operation
        shuttle::thread::yield_now();
        self.poll_pending = false;
        match self.inner.try_recv() {
            Ok(Some(v)) => Ok(v),
            Ok(None) => Err(TryRecvError::Empty),
            Err(_) => Err(TryRecvError::Closed),
        }
    }

    /// Blocking receive to call outside of asynchronous contexts.
    pub fn blocking_recv(self) -> Result<T, RecvError> {
        future::block_on(self)
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        tracing::trace!("dropping oneshot receiver {:p}", self);
        // Scheduling point precedes the visible close of the channel
        thread::switch_if_not_stopping();
    }
}

impl<T> Future for Receiver<T> {
    type Output = Result<T, RecvError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        trace!("polling oneshot receiver {:p}", &self.inner);
        // Scheduling point precedes the read of the channel's state, except after a wake-up
        if !self.poll_pending {
            thread::switch();
        }
        let poll_result = Pin::new(&mut self.inner).poll(cx).map_err(|_| RecvError(()));
        self.poll_pending = poll_result.is_pending();
        poll_result
    }
}
