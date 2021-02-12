use crate::runtime::execution::ExecutionState;
use crate::runtime::task::{TaskId, MAX_INLINE_TASKS};
use crate::runtime::thread;
use smallvec::SmallVec;
use std::cell::RefCell;
use std::fmt::Debug;
use std::rc::Rc;
use std::result::Result;
pub use std::sync::mpsc::{RecvError, RecvTimeoutError, SendError};
use std::sync::Arc;
use std::time::Duration;
use tracing::trace;

// TODO
// * Add support for try_recv() and try_send()
// * Add support for iter() for receivers

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

// Note: The channels in std::sync::mpsc only support a single Receiver (which cannot be
// cloned).  The state below admits a more general use case, where multiple Senders
// and Receivers can share a single channel.
struct ChannelState<T> {
    messages: SmallVec<[T; MAX_INLINE_MESSAGES]>, // messages in the channel
    known_senders: usize,                         // number of senders referencing this channel
    known_receivers: usize,                       // number or receivers referencing this channel
    waiting_senders: SmallVec<[TaskId; MAX_INLINE_TASKS]>, // list of currently blocked senders
    waiting_receivers: SmallVec<[TaskId; MAX_INLINE_TASKS]>, // list of currently blocked receivers
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
        Self {
            bound,
            state: Rc::new(RefCell::new(ChannelState {
                messages: SmallVec::new(),
                known_senders: 1,
                known_receivers: 1,
                waiting_senders: SmallVec::new(),
                waiting_receivers: SmallVec::new(),
            })),
        }
    }

    fn send(&self, message: T) -> Result<(), SendError<T>> {
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
            return Err(SendError(message));
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

        state.waiting_senders.push(me);
        if sender_should_block {
            trace!(
                state = ?state,
                "blocking sender {:?} on channel {:p}",
                me,
                self,
            );
            ExecutionState::with(|s| s.current_mut().block());
            drop(state);

            thread::switch();

            state = self.state.borrow_mut();
            trace!(
                state = ?state,
                "unblocked sender {:?} on channel {:p}",
                me,
                self,
            );

            // Check again that there are no receivers
            // We repeat this check because the receivers may have disconnected while the sender was blocked.
            if state.known_receivers == 0 {
                // No receivers are left, so the channel is disconnected.  Stop and return failure.
                return Err(SendError(message));
            }
        }

        let head = state.waiting_senders.remove(0);
        assert_eq!(head, me);

        state.messages.push(message);
        // The sender has just added a message to the channel, so unblock the first waiting receiver if any
        if let Some(&tid) = state.waiting_receivers.first() {
            ExecutionState::with(|s| s.get_mut(tid).unblock());
        }
        // Check and unblock the next the waiting sender, if eligible
        if let Some(&tid) = state.waiting_senders.first() {
            let bound = self.bound.expect("can't have waiting senders on an unbounded channel");
            if state.messages.len() < bound {
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            }
        }

        Ok(())
    }

    fn recv(&self) -> Result<T, RecvError> {
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
            return Err(RecvError);
        }

        // If this is a rendezvous channel, and the channel is empty, and there are waiting senders,
        // notify the first waiting sender
        if self.bound == Some(0) && state.messages.is_empty() {
            if let Some(&tid) = state.waiting_senders.first() {
                // maybe_unblock because another receiver may have unblocked the sender already
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            }
        }

        // The receiver should block in any of the following situations:
        //    the channel is empty
        //    there are waiting receivers
        let should_block = state.messages.is_empty() || !state.waiting_receivers.is_empty();

        state.waiting_receivers.push(me);
        if should_block {
            trace!(
                state = ?state,
                "blocking receiver {:?} on channel {:p}",
                me,
                self,
            );
            ExecutionState::with(|s| s.current_mut().block());
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
                return Err(RecvError);
            }
        }

        let head = state.waiting_receivers.remove(0);
        assert_eq!(head, me);

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

        Ok(item)
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
    /// corresponding channel has hung up, or if it waits more than timeout.
    pub fn recv_timeout(&self, _timeout: Duration) -> Result<T, RecvTimeoutError> {
        // TODO support the timeout case -- this method never times out
        self.inner.recv().map_err(|_| RecvTimeoutError::Disconnected)
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
