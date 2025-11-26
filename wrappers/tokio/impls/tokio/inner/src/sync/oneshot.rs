//! A one-shot channel is used for sending a single message between
//! asynchronous tasks. The [`channel`] function is used to create a
//! [`Sender`] and [`Receiver`] handle pair that form the channel.

use futures::channel::oneshot;
use shuttle::future;
use std::future::Future;
use std::pin::{pin, Pin};
use std::task::{Context, Poll};
use tracing::trace;

/// Sends a value to the associated [`Receiver`].
#[derive(Debug)]
pub struct Sender<T>(oneshot::Sender<T>);

#[derive(Debug)]
pub struct Receiver<T>(oneshot::Receiver<T>);

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
    (Sender(tx), Receiver(rx))
}

impl<T> Sender<T> {
    /// Attempts to send a value on this channel, returning it back if it could
    /// not be sent.
    pub fn send(self, t: T) -> Result<(), T> {
        trace!("Sending message on oneshot {:p}", &self.0);
        let send_result = self.0.send(t);
        shuttle::thread::yield_now();
        send_result
    }

    /// Waits for the associated [`Receiver`] handle to close.
    pub async fn closed(&mut self) {
        trace!("sender closing oneshot {:p}", &self.0);
        self.0.cancellation().await;
        shuttle::future::yield_now().await;
    }

    /// Returns `true` if the associated [`Receiver`] handle has been dropped.
    pub fn is_closed(&self) -> bool {
        self.0.is_canceled()
    }

    /// Checks whether the oneshot channel has been closed, and if not, schedules the
    /// `Waker` in the provided `Context` to receive a notification when the channel is
    /// closed.
    ///
    /// Note that on multiple calls to poll, only the `Waker` from the `Context` passed
    /// to the most recent call will be scheduled to receive a wakeup.
    pub fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.0.poll_canceled(cx)
    }
}

impl<T> Receiver<T> {
    /// Prevents the associated [`Sender`] handle from sending a value.
    pub fn close(&mut self) {
        self.0.close();
        shuttle::thread::yield_now();
    }

    /// Attempts to receive a value.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        let out = match self.0.try_recv() {
            Ok(Some(v)) => Ok(v),
            Ok(None) => Err(TryRecvError::Empty),
            Err(_) => Err(TryRecvError::Closed),
        };
        shuttle::thread::yield_now();
        out
    }

    /// Blocking receive to call outside of asynchronous contexts.
    pub fn blocking_recv(self) -> Result<T, RecvError> {
        future::block_on(self)
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        tracing::trace!("dropping oneshot receiver {:p}", self);
    }
}

impl<T> Future for Receiver<T> {
    type Output = Result<T, RecvError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let receiver = pin!(&mut self.0);
        trace!("polling oneshot receiver {:p}", receiver);
        let poll_result = receiver.poll(cx).map_err(|_| RecvError(()));
        if poll_result.is_ready() {
            // Force a yield
            shuttle::thread::yield_now();
        }
        poll_result
    }
}
