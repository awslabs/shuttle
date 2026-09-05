//! A multi-producer, single-consumer queue for sending values between
//! asynchronous tasks.

use shuttle::future::{
    self,
    batch_semaphore::{Acquire, BatchSemaphore, Fairness, TryAcquireError},
};
use smallvec::SmallVec;
use std::fmt::{self, Debug};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tracing::trace;

pub use tokio::sync::mpsc::error;
use tokio::sync::mpsc::error::{SendError, TryRecvError, TrySendError};

const MAX_INLINE_MESSAGES: usize = 32;

const PERMIT_ALREADY_USED: &str =
    "Internal Shuttle error. A permit was used after it had already been consumed. This should never happen.";

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

    // Acquires the capacity for one message, blocking until there is room. Callers of `send` must
    // hold capacity acquired this way, and must return it with `release_capacity` if they end up
    // not sending.
    async fn acquire_capacity(&self) -> Result<(), SendError<()>> {
        if self.is_bounded() {
            self.send_semaphore.acquire(1).await.map_err(|_| SendError(()))
        } else if self.is_closed() {
            // An unbounded channel does not take capacity from the `send_semaphore` (nothing ever
            // releases back into it), so the closed check that `acquire` would have done for free
            // has to happen explicitly.
            Err(SendError(()))
        } else {
            Ok(())
        }
    }

    // Like `acquire_capacity`, but fails instead of blocking if the channel is full.
    fn try_acquire_capacity(&self) -> Result<(), TrySendError<()>> {
        if !self.is_bounded() {
            return if self.is_closed() {
                Err(TrySendError::Closed(()))
            } else {
                Ok(())
            };
        }

        match self.send_semaphore.try_acquire(1) {
            Err(TryAcquireError::Closed) => Err(TrySendError::Closed(())),
            Err(TryAcquireError::NoPermits) => Err(TrySendError::Full(())),
            Ok(()) => Ok(()),
        }
    }

    // Returns the capacity for one message to the channel. Called by the receiver once a message
    // has been taken out, and by `Permit`/`OwnedPermit` when a reservation goes unused.
    fn release_capacity(&self) {
        if self.is_bounded() {
            self.send_semaphore.release(1);
        }
    }
}

/// Common building block to build [`Receiver`]/[`UnboundedReceiver`] atop.
struct ReceiverInternal<T> {
    chan: Arc<Channel<T>>,
    /// In-progress semaphore acquire for `poll_recv`. Stored across polls
    /// so the waiter stays registered in the semaphore queue.
    ///
    /// # Safety
    /// The `'static` lifetime is a lie — `Acquire` borrows
    /// `chan.recv_semaphore`. This is sound because we always clear
    /// `pending_acquire` before `chan` is dropped (see `Drop` impl).
    pending_acquire: Option<Pin<Box<Acquire<'static>>>>,
}

impl<T> fmt::Debug for ReceiverInternal<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fmt, "{:?}", self.chan)
    }
}

impl<T> ReceiverInternal<T> {
    pub fn new(chan: Arc<Channel<T>>) -> Self {
        Self {
            chan,
            pending_acquire: None,
        }
    }

    /// Receives the next value for this receiver.
    pub async fn recv(&mut self) -> Option<T> {
        std::future::poll_fn(|cx| self.poll_recv(cx)).await
    }

    /// Tries to receive the next value for this receiver.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        match self.poll_recv(&mut cx) {
            Poll::Ready(Some(item)) => Ok(item),
            Poll::Ready(None) => Err(TryRecvError::Disconnected),
            Poll::Pending => {
                // poll_recv registered a waiter, but try_recv won't poll
                // again — clear it to avoid a dangling waiter.
                self.pending_acquire = None;
                Err(TryRecvError::Empty)
            }
        }
    }

    /// Blocking receive to call outside of asynchronous contexts.
    pub fn blocking_recv(&mut self) -> Option<T> {
        shuttle::future::block_on(self.recv())
    }

    /// Closes the receiving half of a channel, without dropping it.
    pub fn close(&mut self) {
        // Deregister any in-flight waiter before closing.
        self.pending_acquire = None;
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
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        if self.is_closed() && self.is_empty() {
            self.pending_acquire = None;
            return Poll::Ready(None);
        }

        // Create the Acquire future on first poll, reuse on subsequent polls
        // so the waiter stays registered in the semaphore queue.
        if self.pending_acquire.is_none() {
            let acquire = self.chan.recv_semaphore.acquire(1);
            // Safety: Acquire borrows recv_semaphore which lives in self.chan
            // (behind Arc). We clear pending_acquire on completion, on close,
            // and in Drop — so the borrow never outlives the semaphore.
            let acquire: Acquire<'static> = unsafe { std::mem::transmute(acquire) };
            self.pending_acquire = Some(Box::pin(acquire));
        }

        match self.pending_acquire.as_mut().unwrap().as_mut().poll(cx) {
            Poll::Ready(Ok(())) => {
                self.pending_acquire = None;
                let message = self.chan.recv().expect(
                    "Internal Shuttle error. We acquired a permit for an empty channel. This should never happen.",
                );
                self.chan.release_capacity();
                Poll::Ready(Some(message))
            }
            Poll::Ready(Err(_)) => {
                self.pending_acquire = None;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
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
        // Clear pending_acquire before dropping chan so the Acquire's
        // borrow of recv_semaphore is released first.
        self.pending_acquire = None;
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
    pub async fn reserve(&self) -> Result<Permit<'_, T>, SendError<()>> {
        self.chan.acquire_capacity().await?;

        Ok(Permit {
            chan: Some(&*self.chan),
        })
    }

    /// Tries to acquire a slot in the channel without waiting for the slot to
    /// become available.
    pub fn try_reserve(&self) -> Result<Permit<'_, T>, TrySendError<()>> {
        self.chan.try_acquire_capacity()?;

        Ok(Permit {
            chan: Some(&*self.chan),
        })
    }

    /// Waits for channel capacity, moving the `Sender` and returning an owned
    /// permit. Once capacity to send one message is available, it is reserved
    /// for the caller.
    pub async fn reserve_owned(self) -> Result<OwnedPermit<T>, SendError<()>> {
        // An `OwnedPermit` holds a sender slot for as long as it lives, so claim one up front.
        // `self` is dropped when this function returns, giving its own slot back, so
        // `known_senders` is unchanged overall. Claiming before the `await` rather than after
        // matters: it keeps the count from dipping to zero (which would close the channel) while
        // we are waiting for capacity.
        self.claim_sender_slot();
        let chan = Arc::clone(&self.chan);

        if let Err(err) = chan.acquire_capacity().await {
            // Hand the slot we just claimed back. `self` still holds one, so this cannot be the
            // drop that closes the channel.
            chan.drop_sender();
            return Err(err);
        }

        Ok(OwnedPermit { chan: Some(chan) })
    }

    /// Tries to acquire a slot in the channel without waiting for the slot to
    /// become available, moving the `Sender` and returning an owned permit.
    pub fn try_reserve_owned(self) -> Result<OwnedPermit<T>, TrySendError<Self>> {
        match self.chan.try_acquire_capacity() {
            Err(TrySendError::Closed(())) => Err(TrySendError::Closed(self)),
            Err(TrySendError::Full(())) => Err(TrySendError::Full(self)),
            Ok(()) => {
                // See `reserve_owned`. There is no `await` here, so `self`'s slot is live for the
                // whole function and the ordering is not delicate, but the bookkeeping is the same.
                self.claim_sender_slot();

                Ok(OwnedPermit {
                    chan: Some(Arc::clone(&self.chan)),
                })
            }
        }
    }

    // Registers an additional sender on the channel, to be given back with `Channel::drop_sender`.
    fn claim_sender_slot(&self) {
        let mut state = self.chan.state.try_lock().unwrap();
        state.known_senders += 1;
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
        self.claim_sender_slot();

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
        inner: ReceiverInternal {
            chan,
            pending_acquire: None,
        },
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
///
/// The permit holds one message worth of the channel's capacity. Sending consumes it; dropping it
/// without sending returns the capacity to the channel.
//
// `chan` is `None` only after `send` has taken it, which consumes the `Permit`, so every method
// other than `send` and `drop` can rely on it being `Some`.
pub struct Permit<'a, T> {
    chan: Option<&'a Channel<T>>,
}

impl<T> fmt::Debug for Permit<'_, T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("Permit").field("chan", &self.chan).finish()
    }
}

impl<T> Permit<'_, T> {
    /// Sends a value using the reserved capacity.
    pub fn send(mut self, value: T) {
        let chan = self.chan.take().expect(PERMIT_ALREADY_USED);

        // Unlike `Sender::send` this returns `()`, so a channel that closed after the permit was
        // handed out simply drops the value. The capacity is not returned: a closed channel has no
        // capacity to hand out, and nothing is waiting for it.
        if chan.send(value).is_ok() {
            chan.recv_semaphore.release(1);
        }
    }
}

impl<T> Drop for Permit<'_, T> {
    fn drop(&mut self) {
        // `None` here means `send` consumed the permit and the capacity became a message.
        if let Some(chan) = self.chan.take() {
            chan.release_capacity();
        }
    }
}

/// Owned permit to send one value into the channel.
///
/// This is identical to the [`Permit`] type, except that it moves the sender
/// rather than borrowing it.
//
// As well as one message worth of capacity, this holds the moved-in sender's slot in
// `ChannelState::known_senders`. `send` and `release` pass that slot on to the `Sender` they
// return; `drop` gives it back to the channel.
pub struct OwnedPermit<T> {
    chan: Option<Arc<Channel<T>>>,
}

impl<T> fmt::Debug for OwnedPermit<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("OwnedPermit").field("chan", &self.chan).finish()
    }
}

impl<T> OwnedPermit<T> {
    /// Sends a value using the reserved capacity, returning the [`Sender`] the
    /// permit was created from.
    pub fn send(mut self, value: T) -> Sender<T> {
        let chan = self.chan.take().expect(PERMIT_ALREADY_USED);

        // See `Permit::send` for why the error is swallowed.
        if chan.send(value).is_ok() {
            chan.recv_semaphore.release(1);
        }

        Sender {
            inner: SenderInternal { chan },
        }
    }

    /// Releases the reserved capacity without sending a message, returning the
    /// [`Sender`] the permit was created from.
    pub fn release(mut self) -> Sender<T> {
        let chan = self.chan.take().expect(PERMIT_ALREADY_USED);
        chan.release_capacity();

        Sender {
            inner: SenderInternal { chan },
        }
    }

    /// Returns `true` if permits belong to the same channel.
    pub fn same_channel(&self, other: &Self) -> bool {
        // Like tokio, a consumed permit belongs to no channel rather than panicking.
        match (self.chan.as_ref(), other.chan.as_ref()) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Returns `true` if this permit belongs to the same channel as the given
    /// [`Sender`].
    pub fn same_channel_as_sender(&self, sender: &Sender<T>) -> bool {
        match self.chan.as_ref() {
            Some(chan) => Arc::ptr_eq(chan, &sender.inner.chan),
            None => false,
        }
    }
}

impl<T> Drop for OwnedPermit<T> {
    fn drop(&mut self) {
        // `None` here means `send`/`release` handed the sender slot on to a `Sender`.
        if let Some(chan) = self.chan.take() {
            // Return the capacity before giving up the sender slot: `drop_sender` may close the
            // channel, and a task blocked waiting for capacity should see it first.
            chan.release_capacity();
            chan.drop_sender();
        }
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
    pub async fn reserve(&self) -> Result<Permit<'_, T>, SendError<()>> {
        self.inner.reserve().await
    }

    /// Tries to acquire a slot in the channel without waiting for the slot to
    /// become available.
    pub fn try_reserve(&self) -> Result<Permit<'_, T>, TrySendError<()>> {
        self.inner.try_reserve()
    }

    /// Waits for channel capacity, moving the `Sender` and returning an owned
    /// permit. Once capacity to send one message is available, it is reserved
    /// for the caller.
    pub async fn reserve_owned(self) -> Result<OwnedPermit<T>, SendError<()>> {
        self.inner.reserve_owned().await
    }

    /// Tries to acquire a slot in the channel without waiting for the slot to
    /// become available, moving the `Sender` and returning an owned permit.
    ///
    /// If no capacity is available, the `Sender` is returned in the error.
    pub fn try_reserve_owned(self) -> Result<OwnedPermit<T>, TrySendError<Self>> {
        self.inner.try_reserve_owned().map_err(|err| match err {
            TrySendError::Closed(inner) => TrySendError::Closed(Self { inner }),
            TrySendError::Full(inner) => TrySendError::Full(Self { inner }),
        })
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
