//! Multi-producer, single-consumer FIFO queue communication primitives.

use crate::runtime::execution::ExecutionState;
use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::{DEFAULT_INLINE_TASKS, TaskId};
use crate::runtime::thread;
use smallvec::SmallVec;
use std::cell::RefCell;
use std::fmt::Debug;
use std::rc::Rc;
use std::result::Result;
use std::sync::Arc;
pub use std::sync::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError, TrySendError};
use std::time::Duration;
use tracing::trace;

const MAX_INLINE_MESSAGES: usize = 32;

/// Create an unbounded channel
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let channel = Arc::new(Channel::new(None));
    let sender = Sender {
        inner: Arc::clone(&channel),
    };
    let receiver = Receiver {
        inner: Arc::clone(&channel),
    };
    (sender, receiver)
}

/// Create a bounded channel
pub fn sync_channel<T>(bound: usize) -> (SyncSender<T>, Receiver<T>) {
    let channel = Arc::new(Channel::new(Some(bound)));
    let sender = SyncSender {
        inner: Arc::clone(&channel),
    };
    let receiver = Receiver {
        inner: Arc::clone(&channel),
    };
    (sender, receiver)
}

#[derive(Debug)]
struct Channel<T> {
    bound: Option<usize>, // None for an unbounded channel, Some(k) for a bounded channel of size k
    state: Rc<RefCell<ChannelState<T>>>,
}

// For tracking causality on channels, we timestamp each message with the clock of the sender.
// When the receiver gets the message, it updates its clock with the the associated timestamp.
// For unbounded channels, that's all the work we need to do.
//
// For bounded and rendezvous channels, things get a bit more interesting.
// Consider a bounded channel of depth K.  As soon as the sender successfully sends its K+1'th
// message, it knows that the receiver has received at least 1 message.  At this point, the
// first receive event causally precedes the (K+1)'th send.  By the rule for vector clocks,
//  (clock of the first receive)  <  (clock of the K+1'th send)
// In order to ensure this ordering, we add a return queue of depth K to bounded channels.
// Initially, this queue contains K empty vector clocks.  On each receive, we push the
// receiver's clock at the time of the receive to the end of this queue.  Whenever the sender
// successfully sends a message, it pops the clock at the front of the queue, and updates its
// own clock with this value.  Thus, on the (K+1)'th send, the sender's clock will be updated
// with the clock at the first receive, as needed.
//
// The story is similar for rendezvous channels, except we have to handle things a bit more
// specially because K=0.

struct TimestampedValue<T> {
    value: T,
    clock: VectorClock,
}

impl<T> TimestampedValue<T> {
    fn new(value: T, clock: VectorClock) -> Self {
        Self { value, clock }
    }
}

// Note: The channels in std::sync::mpsc only support a single Receiver (which cannot be
// cloned).  The state below admits a more general use case, where multiple Senders
// and Receivers can share a single channel.
struct ChannelState<T> {
    messages: SmallVec<[TimestampedValue<T>; MAX_INLINE_MESSAGES]>, // messages in the channel
    receiver_clock: Option<SmallVec<[VectorClock; MAX_INLINE_MESSAGES]>>, // receiver vector clocks for bounded case
    known_senders: usize,                                           // number of senders referencing this channel
    known_receivers: usize,                                         // number or receivers referencing this channel
    waiting_senders: SmallVec<[TaskId; DEFAULT_INLINE_TASKS]>,      // list of currently blocked senders
    waiting_receivers: SmallVec<[TaskId; DEFAULT_INLINE_TASKS]>,    // list of currently blocked receivers
}

impl<T> Debug for ChannelState<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Channel {{ ")?;
        write!(f, "num_messages: {} ", self.messages.len())?;
        write!(
            f,
            "known_senders {} known_receivers {} ",
            self.known_senders, self.known_receivers
        )?;
        write!(f, "waiting_senders: [{:?}] ", self.waiting_senders)?;
        write!(f, "waiting_receivers: [{:?}] ", self.waiting_receivers)?;
        write!(f, "}}")
    }
}

impl<T> Channel<T> {
    fn new(bound: Option<usize>) -> Self {
        let receiver_clock = if let Some(bound) = bound {
            let mut s = SmallVec::with_capacity(bound);
            for _ in 0..bound {
                s.push(VectorClock::new());
            }
            Some(s)
        } else {
            None
        };
        Self {
            bound,
            state: Rc::new(RefCell::new(ChannelState {
                messages: SmallVec::new(),
                receiver_clock,
                known_senders: 1,
                known_receivers: 1,
                waiting_senders: SmallVec::new(),
                waiting_receivers: SmallVec::new(),
            })),
        }
    }

    fn try_send(&self, message: T) -> Result<(), TrySendError<T>> {
        self.send_internal(message, false)
    }

    fn send(&self, message: T) -> Result<(), SendError<T>> {
        self.send_internal(message, true).map_err(|e| match e {
            TrySendError::Full(_) => unreachable!(),
            TrySendError::Disconnected(m) => SendError(m),
        })
    }

    fn send_internal(&self, message: T, can_block: bool) -> Result<(), TrySendError<T>> {
        let me = ExecutionState::me();
        let mut state = self.state.borrow_mut();

        trace!(
            state = ?state,
            "sender {:?} starting send on channel {:p}",
            me,
            self,
        );
        if state.known_receivers == 0 {
            // No receivers are left, so the channel is disconnected.  Stop and return failure.
            return Err(TrySendError::Disconnected(message));
        }

        let (is_rendezvous, is_full) = if let Some(bound) = self.bound {
            // For a rendezvous channel (bound = 0), "is_full" holds when there is a message in the channel.
            // For a non-rendezvous channel (bound > 0), "is_full" holds when the capacity is reached.
            // We cover both these cases at once using max(bound, 1) below.
            (bound == 0, state.messages.len() >= std::cmp::max(bound, 1))
        } else {
            (false, false)
        };

        // The sender should block in any of the following situations:
        //    the channel is full (as defined above)
        //    there are already waiting senders
        //    this is a rendezvous channel and there are no waiting receivers
        let sender_should_block =
            is_full || !state.waiting_senders.is_empty() || (is_rendezvous && state.waiting_receivers.is_empty());

        if sender_should_block {
            if !can_block {
                return Err(TrySendError::Full(message));
            }

            state.waiting_senders.push(me);
            trace!(
                state = ?state,
                "blocking sender {:?} on channel {:p}",
                me,
                self,
            );
            ExecutionState::with(|s| s.current_mut().block(false));
            drop(state);

            thread::switch();

            state = self.state.borrow_mut();
            trace!(
                state = ?state,
                "unblocked sender {:?} on channel {:p}",
                me,
                self,
            );

            // Check again that we still have a receiver; if not, return with error.
            // We repeat this check because the receivers may have disconnected while the sender was blocked.
            if state.known_receivers == 0 {
                state.waiting_senders.retain(|t| *t != me);
                // No receivers are left, so the channel is disconnected.  Stop and return failure.
                return Err(TrySendError::Disconnected(message));
            }

            let head = state.waiting_senders.remove(0);
            assert_eq!(head, me);
        }

        ExecutionState::with(|s| {
            let clock = s.increment_clock();
            state.messages.push(TimestampedValue::new(message, clock.clone()));
        });

        // The sender has just added a message to the channel, so unblock the first waiting receiver if any
        if let Some(&tid) = state.waiting_receivers.first() {
            ExecutionState::with(|s| {
                s.get_mut(tid).unblock();

                // When a sender successfully sends on a rendezvous channel, it knows that the receiver will perform
                // the matching receive, so we need to update the sender's clock with the receiver's.
                if is_rendezvous {
                    let recv_clock = s.get_clock(tid).clone();
                    s.update_clock(&recv_clock);
                }
            });
        }
        // Check and unblock the next the waiting sender, if eligible
        if let Some(&tid) = state.waiting_senders.first() {
            let bound = self.bound.expect("can't have waiting senders on an unbounded channel");
            if state.messages.len() < bound {
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            }
        }

        if !is_rendezvous {
            if let Some(receiver_clock) = &mut state.receiver_clock {
                let recv_clock = receiver_clock.remove(0);
                ExecutionState::with(|s| s.update_clock(&recv_clock));
            }
        }

        Ok(())
    }

    fn recv(&self) -> Result<T, RecvError> {
        self.recv_internal(true).map_err(|e| match e {
            TryRecvError::Disconnected => RecvError,
            TryRecvError::Empty => unreachable!(),
        })
    }

    fn try_recv(&self) -> Result<T, TryRecvError> {
        self.recv_internal(false)
    }

    fn recv_internal(&self, can_block: bool) -> Result<T, TryRecvError> {
        let me = ExecutionState::me();
        let mut state = self.state.borrow_mut();

        trace!(
            state = ?state,
            "starting recv on channel {:p}",
            self,
        );
        // Check if there are any senders left; if not, and the channel is empty, fail with error
        // (If there are no senders, but the channel is nonempty, the receiver can successfully consume that message.)
        if state.messages.is_empty() && state.known_senders == 0 {
            return Err(TryRecvError::Disconnected);
        }

        let is_rendezvous = self.bound == Some(0);
        // If this is a rendezvous channel, and the channel is empty, and there are waiting senders,
        // notify the first waiting sender
        if is_rendezvous && state.messages.is_empty() {
            if let Some(&tid) = state.waiting_senders.first() {
                // Note: another receiver may have unblocked the sender already
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            } else if !can_block {
                // Nobody to rendezvous with
                return Err(TryRecvError::Empty);
            }
        }

        // Handle the try_recv case, accounting for the number of msgs available and already waiting receivers.
        if !is_rendezvous && !can_block && state.waiting_receivers.len() >= state.messages.len() {
            return Err(TryRecvError::Empty);
        }

        // Pre-increment the receiver's clock before continuing
        //
        // Note: The reason for pre-incrementing the receiver's clock is to deal properly with rendezvous channels.
        // Here's the scenario we have to handle:
        //   1. the receiver arrives at a rendezvous channel and blocks
        //   2. the sender arrives, sees the receiver is waiting and does not block
        //   3. the sender drops the message in the channel and updates its clock with the receiver's clock and continues
        //   4. later, the receiver unblocks and picks up the message and updates its clock with the sender's
        // Without the pre-increment, in step 3, the sender would update its clock with the receiver's clock before
        // it is incremented.  (The increment records the fact that the receiver arrived at the synchronization point.)
        ExecutionState::with(|s| {
            let _ = s.increment_clock();
        });

        // The receiver should block in any of the following situations:
        //    the channel is empty
        //    there are waiting receivers
        let should_block = state.messages.is_empty() || !state.waiting_receivers.is_empty();
        if should_block {
            state.waiting_receivers.push(me);
            trace!(
                state = ?state,
                "blocking receiver {:?} on channel {:p}",
                me,
                self,
            );
            ExecutionState::with(|s| s.current_mut().block(false));
            drop(state);

            thread::switch();

            state = self.state.borrow_mut();
            trace!(
                state = ?state,
                "unblocked receiver {:?} on channel {:p}",
                me,
                self,
            );

            // Check again if there are any senders left; if not, and the channel is empty, fail with error
            // (If there are no senders, but the channel is nonempty, the receiver can successfully consume that message.)
            // We repeat this check because the senders may have disconnected while the receiver was blocked.
            if state.messages.is_empty() && state.known_senders == 0 {
                state.waiting_receivers.retain(|t| *t != me);
                return Err(TryRecvError::Disconnected);
            }

            let head = state.waiting_receivers.remove(0);
            assert_eq!(head, me);
        }

        let item = state.messages.remove(0);
        // The receiver has just removed an element from the channel.  Check if any waiting senders
        // need to be notified.
        if let Some(&tid) = state.waiting_senders.first() {
            let bound = self.bound.expect("can't have waiting senders on an unbounded channel");
            // Unblock the first waiting sender provided one of the following conditions hold:
            // - this is a non-rendezvous bounded channel (bound > 0)
            // - this is a rendezvous channel and we have additional waiting receivers
            if bound > 0 || !state.waiting_receivers.is_empty() {
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            }
        }
        // Check and unblock the next the waiting receiver, if eligible
        // Note: this is a no-op for mpsc channels, since there can only be one receiver
        if let Some(&tid) = state.waiting_receivers.first() {
            if !state.messages.is_empty() {
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            }
        }

        // Update receiver clock from the clock attached to the message received
        let TimestampedValue { value, clock } = item;
        ExecutionState::with(|s| {
            // Since we already incremented the receiver's clock above, just update it here
            s.get_clock_mut(me).update(&clock);

            // If this is a (non-rendezvous) bounded channel, propagate causality backwards to sender
            if let Some(receiver_clock) = &mut state.receiver_clock {
                let bound = self.bound.expect("unexpected internal error"); // must be defined for bounded channels
                if bound > 0 {
                    // non-rendezvous
                    assert!(receiver_clock.len() < bound);
                    receiver_clock.push(s.get_clock(me).clone());
                }
            }
        });
        Ok(value)
    }
}

// Safety: A Channel is never actually passed across true threads, only across continuations. The
// Rc<RefCell<_>> type therefore can't be preempted mid-bookkeeping-operation.
// TODO We use this workaround in several places in Shuttle.  Maybe there's a cleaner solution.
unsafe impl<T: Send> Send for Channel<T> {}
unsafe impl<T: Send> Sync for Channel<T> {}

/// The receiving half of Rust's [`channel`] (or [`sync_channel`]) type.
/// This half can only be owned by one thread.
#[derive(Debug)]
pub struct Receiver<T> {
    inner: Arc<Channel<T>>,
}

impl<T> Receiver<T> {
    /// Attempts to wait for a value on this receiver, returning an error if the
    /// corresponding channel has hung up.
    pub fn recv(&self) -> Result<T, RecvError> {
        self.inner.recv()
    }

    /// Attempts to wait for a value on this receiver, returning an error if the
    /// corresponding channel has hung up.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.inner.try_recv()
    }

    /// Attempts to wait for a value on this receiver, returning an error if the
    /// corresponding channel has hung up, or if it waits more than timeout.
    pub fn recv_timeout(&self, _timeout: Duration) -> Result<T, RecvTimeoutError> {
        // TODO support the timeout case -- this method never times out
        self.inner.recv().map_err(|_| RecvTimeoutError::Disconnected)
    }

    /// Returns an iterator that will block waiting for messages, but never
    /// [`panic!`]. It will return [`None`] when the channel has hung up.
    pub fn iter(&self) -> Iter<'_, T> {
        Iter { rx: self }
    }

    /// Returns an iterator that will attempt to yield all pending values.
    /// It will return `None` if there are no more pending values or if the
    /// channel has hung up. The iterator will never [`panic!`] or block the
    /// user by waiting for values.
    pub fn try_iter(&self) -> TryIter<'_, T> {
        TryIter { rx: self }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        if ExecutionState::should_stop() {
            return;
        }
        let mut state = self.inner.state.borrow_mut();
        assert!(state.known_receivers > 0);
        state.known_receivers -= 1;
        if state.known_receivers == 0 {
            // Last receiver was dropped; wake up all senders
            for &tid in state.waiting_senders.iter() {
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            }
        }
    }
}

/// An iterator over messages on a [`Receiver`], created by [`iter`].
///
/// This iterator will block whenever [`next`] is called,
/// waiting for a new message, and [`None`] will be returned
/// when the corresponding channel has hung up.
///
/// [`iter`]: Receiver::iter
/// [`next`]: Iterator::next
#[derive(Debug)]
pub struct Iter<'a, T: 'a> {
    rx: &'a Receiver<T>,
}

/// An iterator that attempts to yield all pending values for a [`Receiver`],
/// created by [`try_iter`].
///
/// [`None`] will be returned when there are no pending values remaining or
/// if the corresponding channel has hung up.
///
/// This iterator will never block the caller in order to wait for data to
/// become available. Instead, it will return [`None`].
///
/// [`try_iter`]: Receiver::try_iter
#[derive(Debug)]
pub struct TryIter<'a, T: 'a> {
    rx: &'a Receiver<T>,
}

/// An owning iterator over messages on a [`Receiver`],
/// created by [`into_iter`].
///
/// This iterator will block whenever [`next`]
/// is called, waiting for a new message, and [`None`] will be
/// returned if the corresponding channel has hung up.
///
/// [`into_iter`]: Receiver::into_iter
/// [`next`]: Iterator::next
#[derive(Debug)]
pub struct IntoIter<T> {
    rx: Receiver<T>,
}

impl<T> Iterator for Iter<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.rx.recv().ok()
    }
}

impl<T> Iterator for TryIter<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.rx.try_recv().ok()
    }
}

impl<'a, T> IntoIterator for &'a Receiver<T> {
    type Item = T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Iter<'a, T> {
        self.iter()
    }
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.rx.recv().ok()
    }
}

impl<T> IntoIterator for Receiver<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> IntoIter<T> {
        IntoIter { rx: self }
    }
}

/// The sending-half of Rust's asynchronous [`channel`] type. This half can only be
/// owned by one thread, but it can be cloned to send to other threads.
#[derive(Debug)]
pub struct Sender<T> {
    inner: Arc<Channel<T>>,
}

impl<T> Sender<T> {
    /// Attempts to send a value on this channel, returning it back if it could
    /// not be sent.
    pub fn send(&self, t: T) -> Result<(), SendError<T>> {
        self.inner.send(t)
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        let mut state = self.inner.state.borrow_mut();
        state.known_senders += 1;
        drop(state);
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if ExecutionState::should_stop() {
            return;
        }
        let mut state = self.inner.state.borrow_mut();
        assert!(state.known_senders > 0);
        state.known_senders -= 1;
        if state.known_senders == 0 {
            // Last sender was dropped; wake up all receivers
            for &tid in state.waiting_receivers.iter() {
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            }
        }
    }
}

/// The sending-half of Rust's synchronous [`sync_channel`] type.
///
/// Messages can be sent through this channel with [`SyncSender::send`] or \[`try_send`\] (TODO)
///
/// [`SyncSender::send`] will block if there is no space in the internal buffer.
#[derive(Debug)]
pub struct SyncSender<T> {
    inner: Arc<Channel<T>>,
}

impl<T> SyncSender<T> {
    /// Sends a value on this synchronous channel.
    ///
    /// This function will *block* until space in the internal buffer becomes
    /// available or a receiver is available to hand off the message to.
    pub fn send(&self, t: T) -> Result<(), SendError<T>> {
        self.inner.send(t)
    }

    /// Attempts to send a value on this channel without blocking.
    ///
    /// This method differs from [`send`] by returning immediately if the
    /// channel's buffer is full or no receiver is waiting to acquire some
    /// data. Compared with [`send`], this function has two failure cases
    /// instead of one (one for disconnection, one for a full buffer).
    ///
    /// [`send`]: Self::send
    pub fn try_send(&self, t: T) -> Result<(), TrySendError<T>> {
        self.inner.try_send(t)
    }
}

impl<T> Clone for SyncSender<T> {
    fn clone(&self) -> Self {
        let mut state = self.inner.state.borrow_mut();
        state.known_senders += 1;
        drop(state);
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for SyncSender<T> {
    fn drop(&mut self) {
        if ExecutionState::should_stop() {
            return;
        }
        let mut state = self.inner.state.borrow_mut();
        assert!(state.known_senders > 0);
        state.known_senders -= 1;
        if state.known_senders == 0 {
            // Last sender was dropped; wake up any receivers
            for &tid in state.waiting_receivers.iter() {
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            }
        }
    }
}
