//! A multi-producer, single-consumer queue for sending values between
//! asynchronous tasks.

use shuttle::future::{
    self,
    batch_semaphore::{BatchSemaphore, Fairness, TryAcquireError},
};
use smallvec::SmallVec;
use std::fmt::{self, Debug};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tracing::trace;

pub use tokio::sync::mpsc::error;
use tokio::sync::mpsc::error::{SendError, TryRecvError, TrySendError};

const MAX_INLINE_MESSAGES: usize = 32;

// === Base Channel ===

struct Channel<T> {
    // If all senders have left and the channel is empty, we want to ensure that the receiver is
    // not blocked.  To ensure this, we'll maintain the following invariant
    //     (state.known_senders == 0 && state.messages.is_empty()) == (recv_semaphore is closed)
    bound: Option<usize>, // None for an unbounded channel, Some(k) for bounded channel of size k
    recv_semaphore: Arc<BatchSemaphore>, // semaphore used to signal receivers
    send_semaphore: Arc<BatchSemaphore>, // semaphore used to block senders. Also tracks whether the channel is closed for sending messages.
    state: Arc<Mutex<ChannelState<T>>>,
}

impl<T> Debug for Channel<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Channel {{ ")?;
        write!(f, "recv_semaphore: {:?} ", self.recv_semaphore)?;
        write!(f, "send_semaphore: {:?} ", self.send_semaphore)?;
        write!(f, "state: {:?} ", self.state)?;
        write!(f, "}}")
    }
}

struct ChannelState<T> {
    messages: SmallVec<[T; MAX_INLINE_MESSAGES]>, // messages in the channel
    known_senders: usize,                         // number of senders referencing this channel
}

impl<T> Debug for ChannelState<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ChannelState {{ ")?;
        write!(f, "num_messages: {} ", self.messages.len())?;
        write!(f, "known_senders {}", self.known_senders,)?;
        write!(f, "}}")
    }
}

impl<T> Channel<T> {
    fn new(bound: Option<usize>) -> Self {
        let recv_semaphore = Arc::new(BatchSemaphore::new(0, Fairness::StrictlyFair));
        let send_semaphore = Arc::new(BatchSemaphore::new(bound.unwrap_or(usize::MAX), Fairness::StrictlyFair));

        Self {
            bound,
            recv_semaphore,
            send_semaphore,
            state: Arc::new(Mutex::new(ChannelState {
                messages: SmallVec::new(),
                known_senders: 1,
            })),
        }
    }

    // Send a message on the channel.  Note that callers of this method must ensure that
    // the channel has enough capacity for the send to be successful.
    fn send(&self, message: T) -> Result<(), SendError<T>> {
        if self.is_closed() {
            return Err(SendError(message));
        }

        let mut state = self.state.try_lock().unwrap();

        if let Some(bound) = self.bound {
            assert!(state.messages.len() < bound);
        }

        state.messages.push(message);
        trace!(
            "sent message on channel {:p} num_messages {}",
            self,
            state.messages.len()
        );

        Ok(())
    }

    // Receive a message from the channel if one is available
    fn recv(&self) -> Option<T> {
        let mut state = self.state.try_lock().unwrap();
        trace!(
            "receiving message on channel {:p} with {} messages",
            self,
            state.messages.len()
        );

        // TODO / nit: If we update `is_empty` / `len` / `close` to be `VectorClock`ed functions, then the code below will have wasteful clock work.
        if state.messages.is_empty() {
            None
        } else {
            let msg = Some(state.messages.remove(0));

            if state.messages.is_empty() && state.known_senders == 0 {
                trace!(
                    "closing receiving semaphore {:p} for channel {:p} after having drained the channel post last sender drop",
                    self.recv_semaphore,
                    self
                );

                // `close` is a scheduling point, so we need to release the lock on `state` here
                drop(state);

                // To ensure the invariant above; when the receiver picks up the last message
                // from a channel with no senders, it closes the recv_semaphore
                self.recv_semaphore.close();
            }

            msg
        }
    }

    fn is_closed(&self) -> bool {
        self.send_semaphore.is_closed()
    }

    fn close(&self) {
        trace!(
            "closing sending semaphore {:p} for channel {:p}",
            self.send_semaphore,
            self
        );
        self.send_semaphore.close();
    }

    fn drop_receiver(&self) {
        trace!("closing channel {:p} on receiver drop", self);

        self.close();

        // need to drop after releasing lock and closing semaphore to avoid deadlocks
        let _unreceived_messages_to_drop = std::mem::take(&mut self.state.try_lock().unwrap().messages);
    }

    fn drop_sender(&self) {
        // Note that we deliberately limit how long we are holding the lock both here and below.
        // We have to do this because `BatchSemaphore::close` is a scheduling point. If we were to hold
        // the Mutex across a scheduling point, then we run the risk of trying to reacquire the lock,
        // deadlocking on ourself.
        let known_senders = {
            let mut state = self.state.try_lock().unwrap();
            trace!(
                "dropping sender for channel {:p} at count {:?}",
                self,
                state.known_senders
            );

            assert!(state.known_senders > 0);
            state.known_senders -= 1;
            state.known_senders
        };

        if known_senders == 0 {
            self.close();

            let no_messages_in_channel = {
                let state = self.state.try_lock().unwrap();
                state.messages.is_empty()
            };

            // If there are messages, then the `recv_semaphore` will remain open until the last message is `recv`d.
            if no_messages_in_channel {
                trace!("closing semaphore {:p} on last sender drop", self.recv_semaphore);
                // See invariant above; when the last sender leaves an empty channel, it
                // closes the recv_semaphore
                self.recv_semaphore.close_no_scheduling_point();
            }
        }
    }

    // TODO: This must be VectorClocked right? If not then we can use this as an AtomicBool/AtomicUsize without any clocking.
    /// Returns the number of messages in the channel.
    fn len(&self) -> usize {
        self.state.try_lock().unwrap().messages.len()
    }

    /// Checks if the channel is empty.
    ///
    /// This method returns `true` if the channel has no messages.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn is_bounded(&self) -> bool {
        self.bound.is_some()
    }
}

/// Common building block to build [`Receiver`]/[`UnboundedReceiver`] atop.
struct ReceiverInternal<T> {
    chan: Arc<Channel<T>>,
}

impl<T> fmt::Debug for ReceiverInternal<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fmt, "{:?}", self.chan)
    }
}

impl<T> ReceiverInternal<T> {
    pub fn new(chan: Arc<Channel<T>>) -> Self {
        Self { chan }
    }

    /// Receives the next value for this receiver.
    pub async fn recv(&mut self) -> Option<T> {
        if self.is_closed() && self.is_empty() {
            return None;
        }

        self.chan.recv_semaphore.acquire(1).await.ok()?;
        let message = self.chan.recv()?;

        if self.chan.is_bounded() {
            self.chan.send_semaphore.release(1);
        }

        Some(message)
    }

    /// Tries to receive the next value for this receiver.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        match self.chan.recv_semaphore.try_acquire(1) {
            Err(TryAcquireError::Closed) => Err(TryRecvError::Disconnected),
            Err(TryAcquireError::NoPermits) => Err(TryRecvError::Empty),
            Ok(()) => {
                let message = self.chan.recv().expect(
                    "Internal Shuttle error. We acquired a permit for an empty channel. This should never happen.",
                );
                if self.chan.is_bounded() {
                    self.chan.send_semaphore.release(1);
                }
                Ok(message)
            }
        }
    }

    /// Blocking receive to call outside of asynchronous contexts.
    pub fn blocking_recv(&mut self) -> Option<T> {
        if self.is_closed() && self.is_empty() {
            return None;
        }

        self.chan.recv_semaphore.acquire_blocking(1).ok()?;
        self.chan.recv()
    }

    /// Closes the receiving half of a channel, without dropping it.
    pub fn close(&mut self) {
        self.chan.close();
    }

    /// Checks if a channel is closed.
    ///
    /// This method returns `true` if the channel has been closed. The channel is closed
    /// when all [`UnboundedSender`] have been dropped, or when [`UnboundedReceiver::close`] is called.
    pub fn is_closed(&self) -> bool {
        self.chan.is_closed()
    }

    /// Polls to receive the next message on this channel.
    ///
    /// This method returns:
    ///
    ///  * `Poll::Pending` if no messages are available but the channel is not
    ///    closed, or if a spurious failure happens.
    ///  * `Poll::Ready(Some(message))` if a message is available.
    ///  * `Poll::Ready(None)` if the channel has been closed and all messages
    ///    sent before it was closed have been received.
    pub fn poll_recv(&mut self, _cx: &mut Context<'_>) -> Poll<Option<T>> {
        unimplemented!()
    }

    /// Checks if a channel is empty.
    ///
    /// This method returns `true` if the channel has no messages.
    pub fn is_empty(&self) -> bool {
        self.chan.is_empty()
    }

    /// Returns the number of messages in the channel.
    pub fn len(&self) -> usize {
        self.chan.len()
    }
}

impl<T> Drop for ReceiverInternal<T> {
    fn drop(&mut self) {
        self.chan.drop_receiver();
    }
}

/// Common building block to build [`Sender`]/[`UnboundedSender`] atop.
struct SenderInternal<T> {
    chan: Arc<Channel<T>>,
}

impl<T> fmt::Debug for SenderInternal<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fmt, "{:?}", self.chan)
    }
}

impl<T> SenderInternal<T> {
    fn new(chan: Arc<Channel<T>>) -> Self {
        Self { chan }
    }

    /// Sends a value, waiting until there is capacity.
    pub async fn send(&self, message: T) -> Result<(), SendError<T>> {
        if self.chan.is_bounded() {
            match self.chan.send_semaphore.acquire(1).await {
                Ok(()) => {}
                Err(_) => return Err(SendError(message)),
            }
        }

        self.chan.send(message)?;
        self.chan.recv_semaphore.release(1);

        Ok(())
    }

    /// Completes when the receiver has dropped.
    pub async fn closed(&self) {
        unimplemented!()
    }

    /// Attempts to immediately send a message on this `Sender`
    pub fn try_send(&self, message: T) -> Result<(), TrySendError<T>> {
        match self.chan.send_semaphore.try_acquire(1) {
            Err(TryAcquireError::Closed) => Err(TrySendError::Closed(message)),
            Err(TryAcquireError::NoPermits) => Err(TrySendError::Full(message)),
            Ok(()) => {
                self.chan.send(message)?;
                self.chan.recv_semaphore.release(1);
                Ok(())
            }
        }
    }

    /// Blocking send to call outside of asynchronous contexts.
    pub fn blocking_send(&self, message: T) -> Result<(), SendError<T>> {
        future::block_on(self.send(message))
    }

    /// Checks if the channel has been closed. This happens when the
    /// [`Receiver`] is dropped, or when the [`Receiver::close`] method is
    /// called.
    pub fn is_closed(&self) -> bool {
        self.chan.is_closed()
    }

    /// Waits for channel capacity. Once capacity to send one message is
    /// available, it is reserved for the caller.
    pub async fn reserve(&self) -> Result<Permit<T>, SendError<()>> {
        unimplemented!()
    }

    /// Waits for channel capacity, moving the `Sender` and returning an owned
    /// permit. Once capacity to send one message is available, it is reserved
    /// for the caller.
    pub async fn reserve_owned(self) -> Result<OwnedPermit<T>, SendError<()>> {
        unimplemented!()
    }

    /// Returns `true` if senders belong to the same channel.
    pub fn same_channel(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.chan, &other.chan)
    }

    /// Returns the current capacity of the channel.
    pub fn capacity(&self) -> usize {
        self.chan.send_semaphore.available_permits()
    }

    /// Returns the maximum buffer capacity of the channel.
    pub fn max_capacity(&self) -> usize {
        match self.chan.bound {
            None => usize::MAX,
            Some(k) => k,
        }
    }
}

impl<T> Clone for SenderInternal<T> {
    fn clone(&self) -> Self {
        {
            let mut state = self.chan.state.try_lock().unwrap();
            state.known_senders += 1;
        }

        SenderInternal {
            chan: self.chan.clone(),
        }
    }
}

impl<T> Drop for SenderInternal<T> {
    fn drop(&mut self) {
        self.chan.drop_sender();
    }
}

// === Unbounded Channel ===

/// Receive values from the associated `UnboundedSender`.
pub struct UnboundedReceiver<T> {
    inner: ReceiverInternal<T>,
}

impl<T> fmt::Debug for UnboundedReceiver<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("UnboundedReceiver")
            .field("chan", &self.inner)
            .finish()
    }
}

/// Creates an unbounded mpsc channel for communicating between asynchronous
/// tasks without backpressure.
pub fn unbounded_channel<T>() -> (UnboundedSender<T>, UnboundedReceiver<T>) {
    let chan = Arc::new(Channel::new(None));
    let sender = UnboundedSender {
        inner: SenderInternal::new(chan.clone()),
    };
    let receiver = UnboundedReceiver {
        inner: ReceiverInternal::new(chan),
    };
    (sender, receiver)
}

impl<T> UnboundedReceiver<T> {
    /// Receives the next value for this receiver.
    pub async fn recv(&mut self) -> Option<T> {
        self.inner.recv().await
    }

    /// Tries to receive the next value for this receiver.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        self.inner.try_recv()
    }

    /// Blocking receive to call outside of asynchronous contexts.
    pub fn blocking_recv(&mut self) -> Option<T> {
        self.inner.blocking_recv()
    }

    /// Closes the receiving half of a channel, without dropping it.
    pub fn close(&mut self) {
        self.inner.close();
    }

    /// Checks if a channel is closed.
    ///
    /// This method returns `true` if the channel has been closed. The channel is closed
    /// when all [`UnboundedSender`] have been dropped, or when [`UnboundedReceiver::close`] is called.
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    /// Polls to receive the next message on this channel.
    ///
    /// This method returns:
    ///
    ///  * `Poll::Pending` if no messages are available but the channel is not
    ///    closed, or if a spurious failure happens.
    ///  * `Poll::Ready(Some(message))` if a message is available.
    ///  * `Poll::Ready(None)` if the channel has been closed and all messages
    ///    sent before it was closed have been received.
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        self.inner.poll_recv(cx)
    }

    /// Checks if a channel is empty.
    ///
    /// This method returns `true` if the channel has no messages.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns the number of messages in the channel.
    pub fn len(&self) -> usize {
        self.inner.len()
    }
}

// == UnboundedSender ==

/// Send values to the associated `UnboundedReceiver`.
pub struct UnboundedSender<T> {
    inner: SenderInternal<T>,
}

// Note that this cannot be derived, as then we get a `T: Clone` bound, but `UnboundedSender` should
// be `Clone` even if `T` is not
impl<T> Clone for UnboundedSender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> fmt::Debug for UnboundedSender<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("UnboundedSender").field("chan", &self.inner).finish()
    }
}

impl<T> UnboundedSender<T> {
    /// Attempts to send a message on this `UnboundedSender` without blocking.
    pub fn send(&self, message: T) -> Result<(), SendError<T>> {
        future::block_on(self.inner.send(message))
    }

    /// Completes when the receiver has dropped.
    pub async fn closed(&self) {
        self.inner.closed().await;
    }

    /// Checks if the channel has been closed. This happens when the
    /// [`UnboundedReceiver`] is dropped, or when the
    /// [`UnboundedReceiver::close`] method is called.
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    /// Returns `true` if senders belong to the same channel.
    pub fn same_channel(&self, other: &Self) -> bool {
        self.inner.same_channel(&other.inner)
    }
}

// ==== BOUNDED CHANNEL

/// Receives values from the associated `Sender`.
pub struct Receiver<T> {
    inner: ReceiverInternal<T>,
}

impl<T> fmt::Debug for Receiver<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("Receiver").field("chan", &self.inner).finish()
    }
}

/// Creates a bounded mpsc channel for communicating between asynchronous tasks
/// with backpressure.
///
/// The channel will buffer up to the provided number of messages.  Once the
/// buffer is full, attempts to send new messages will wait until a message is
/// received from the channel. The provided buffer capacity must be at least 1.
///
/// All data sent on `Sender` will become available on `Receiver` in the same
/// order as it was sent.
///
/// The `Sender` can be cloned to `send` to the same channel from multiple code
/// locations. Only one `Receiver` is supported.
///
/// If the `Receiver` is disconnected while trying to `send`, the `send` method
/// will return a `SendError`. Similarly, if `Sender` is disconnected while
/// trying to `recv`, the `recv` method will return `None`.
pub fn channel<T>(bound: usize) -> (Sender<T>, Receiver<T>) {
    let chan = Arc::new(Channel::new(Some(bound)));
    let sender = Sender {
        inner: SenderInternal::new(chan.clone()),
    };
    let receiver = Receiver {
        inner: ReceiverInternal { chan },
    };
    (sender, receiver)
}

impl<T> Receiver<T> {
    /// Receives the next value for this receiver.
    pub async fn recv(&mut self) -> Option<T> {
        self.inner.recv().await
    }

    /// Tries to receive the next value for this receiver.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        self.inner.try_recv()
    }

    /// Blocking receive to call outside of asynchronous contexts.
    pub fn blocking_recv(&mut self) -> Option<T> {
        self.inner.blocking_recv()
    }

    /// Closes the receiving half of a channel, without dropping it.
    pub fn close(&mut self) {
        self.inner.close();
    }

    /// Polls to receive the next message on this channel.
    ///
    /// This method returns:
    ///
    ///  * `Poll::Pending` if no messages are available but the channel is not
    ///    closed, or if a spurious failure happens.
    ///  * `Poll::Ready(Some(message))` if a message is available.
    ///  * `Poll::Ready(None)` if the channel has been closed and all messages
    ///    sent before it was closed have been received.
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        self.inner.poll_recv(cx)
    }

    /// Returns the number of messages in the channel.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Checks if a channel is empty.
    ///
    /// This method returns `true` if the channel has no messages.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Checks if a channel is closed.
    ///
    /// This method returns `true` if the channel has been closed. The channel is closed
    /// when all [`Sender`] have been dropped, or when [`Receiver::close`] is called.
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

impl<T> Unpin for Receiver<T> {}

// === BOUNDED SENDER ===

/// Sends values to the associated `Receiver`.
pub struct Sender<T> {
    inner: SenderInternal<T>,
}

// Note that this cannot be derived, as then we get a `T: Clone` bound, but `Sender` should
// be `Clone` even if `T` is not
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// Permits to send one value into the channel.
pub struct Permit<T> {
    chan: Arc<Channel<T>>,
}

impl<T> fmt::Debug for Permit<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("Permit").field("chan", &self.chan).finish()
    }
}

/// Owned permit to send one value into the channel.
///
/// This is identical to the [`Permit`] type, except that it moves the sender
/// rather than borrowing it.
pub struct OwnedPermit<T> {
    chan: Option<Arc<Channel<T>>>,
}

impl<T> fmt::Debug for OwnedPermit<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("OwnedPermit").field("chan", &self.chan).finish()
    }
}

impl<T> Sender<T> {
    /// Sends a value, waiting until there is capacity.
    pub async fn send(&self, message: T) -> Result<(), SendError<T>> {
        self.inner.send(message).await
    }

    /// Completes when the receiver has dropped.
    pub async fn closed(&self) {
        self.inner.closed().await;
    }

    /// Attempts to immediately send a message on this `Sender`
    pub fn try_send(&self, message: T) -> Result<(), TrySendError<T>> {
        self.inner.try_send(message)
    }

    /// Blocking send to call outside of asynchronous contexts.
    pub fn blocking_send(&self, message: T) -> Result<(), SendError<T>> {
        self.inner.blocking_send(message)
    }

    /// Checks if the channel has been closed. This happens when the
    /// [`Receiver`] is dropped, or when the [`Receiver::close`] method is
    /// called.
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    /// Waits for channel capacity. Once capacity to send one message is
    /// available, it is reserved for the caller.
    pub async fn reserve(&self) -> Result<Permit<T>, SendError<()>> {
        self.inner.reserve().await
    }

    /// Waits for channel capacity, moving the `Sender` and returning an owned
    /// permit. Once capacity to send one message is available, it is reserved
    /// for the caller.
    pub async fn reserve_owned(self) -> Result<OwnedPermit<T>, SendError<()>> {
        self.inner.reserve_owned().await
    }

    /// Returns `true` if senders belong to the same channel.
    pub fn same_channel(&self, other: &Self) -> bool {
        self.inner.same_channel(&other.inner)
    }

    /// Returns the current capacity of the channel.
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    /// Returns the maximum buffer capacity of the channel.
    pub fn max_capacity(&self) -> usize {
        self.inner.max_capacity()
    }
}

impl<T> fmt::Debug for Sender<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("Sender").field("chan", &self.inner).finish()
    }
}
