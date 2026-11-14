//! The channels that carry messages: unbounded, bounded, and bounded with zero capacity.
//!
//! # How tasks wait
//!
//! A task whose operation cannot complete registers a [`Waiter`] with the channel, and blocks. What
//! wakes it depends on whether the channel has a buffer:
//!
//! * **Unbounded and bounded channels** wake every task blocked on a side when that side may be
//!   able to go on: all the receivers when a message arrives, all the senders when a message
//!   leaves a full channel, and everyone when the channel disconnects. The woken tasks try again,
//!   in the order the scheduler runs them, so the scheduler decides which of several receivers gets
//!   a message, and a task that has not blocked yet can take it before any of them. crossbeam
//!   wakes the receiver that has waited longest, but does not promise which receiver gets the
//!   message, and also lets a newcomer take it first.
//! * **Zero-capacity channels** hand a message from one task to another directly, as crossbeam's
//!   do. A blocked task offers its operation, and the task that arrives on the other side completes
//!   it with the operation that has waited longest, as crossbeam's does, and wakes its task. A task
//!   never rendezvouses with itself.
//!
//! # Causality
//!
//! A message carries the clock of its sender, which its receiver joins. A sender of a bounded
//! channel joins the clock of the receive that freed the place its message takes, and a sender of
//! a zero-capacity channel the clock of the receiver it meets. An operation that fails because the
//! channel disconnected joins the clock of the task that disconnected it.

use crate::generation;
use crate::select::{Token, TokenKind};
use crate::waiter::{self, Waiter};
use crossbeam_channel::{RecvTimeoutError, SendTimeoutError, TryRecvError, TrySendError};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::task::TaskId;
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::thread;
use shuttle_time_inner::time::Instant;
use shuttle_time_inner::timer_support::peek;
use std::cell::{RefCell, RefMut};
use std::collections::VecDeque;
use std::rc::Rc;

/// Which kind of channel a [`Chan`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flavor {
    /// `unbounded()`.
    List,
    /// `bounded(cap)`, with `cap > 0`.
    Array(usize),
    /// `bounded(0)`.
    Zero,
}

/// A message, with the clock of the task that sent it.
pub(crate) struct Message<T> {
    pub(crate) value: T,
    pub(crate) clock: VectorClock,
}

/// The clock of the current task, after counting the event of sending a message.
fn send_clock() -> VectorClock {
    ExecutionState::with(|state| state.increment_clock().clone())
}

/// The clock of the current task.
fn my_clock() -> VectorClock {
    shuttle_engine::current::clock()
}

/// Makes the current task causally depend on whatever `clock` has seen.
pub(crate) fn join(clock: &VectorClock) {
    ExecutionState::with(|state| state.update_clock(clock));
}

/// Where a zero-capacity channel hands a message over from a sender to a receiver.
pub(crate) struct Slot<T> {
    /// The message, while it is on its way.
    message: RefCell<Option<Message<T>>>,
    /// For a blocked send (`Kind::Offer`): the clock of the receiver that took its message, for the
    /// sender to join.
    receiver_clock: RefCell<Option<VectorClock>>,
    /// For a blocked select's send arm (`Kind::OfferLater`): the receiver that claimed it, which
    /// waits for the message in a slot of its own.
    claimed_by: RefCell<Option<(Rc<Slot<T>>, TaskId)>>,
}

impl<T> Slot<T> {
    fn empty() -> Self {
        Self {
            message: RefCell::new(None),
            receiver_clock: RefCell::new(None),
            claimed_by: RefCell::new(None),
        }
    }

    fn holding(message: Message<T>) -> Self {
        let slot = Self::empty();
        *slot.message.borrow_mut() = Some(message);
        slot
    }

    /// Waits until a sender has put the message in this receiver's slot, and takes it.
    pub(crate) fn await_message(&self) -> Message<T> {
        loop {
            if let Some(message) = self.message.borrow_mut().take() {
                return message;
            }
            // The sender wakes this task once it has written the message (see `Chan::deliver`).
            waiter::block(None);
        }
    }

    /// Puts a message in a receiver's slot, and wakes the receiver. A rendezvous: the sender also
    /// learns that the receiver has arrived.
    fn deliver(&self, receiver: TaskId, value: T) {
        let clock = send_clock();
        *self.message.borrow_mut() = Some(Message { value, clock });
        let receiver_clock = ExecutionState::with(|state| state.get_clock(receiver).clone());
        join(&receiver_clock);
        waiter::wake(receiver);
    }
}

/// What a blocked operation registered with one side of a channel.
enum Kind<T> {
    /// It waits for the channel to change, and then tries again: every operation of a channel
    /// with a buffer, and every wait for readiness.
    Retry,
    /// Zero capacity: a blocked send, whose message is in the slot.
    Offer(Rc<Slot<T>>),
    /// Zero capacity: a blocked select's send arm, which has no message yet. The receiver that
    /// claims it leaves its own slot here, and the select writes the message there once it has
    /// worked out what the message is.
    OfferLater(Rc<Slot<T>>),
    /// Zero capacity: a blocked receive, whose sender puts the message in the slot.
    Accept(Rc<Slot<T>>),
}

struct Entry<T> {
    waiter: Rc<Waiter>,
    /// Which of the waiter's operations this is.
    op: usize,
    kind: Kind<T>,
}

impl<T> Entry<T> {
    /// Whether a counterpart can complete or claim this operation now.
    fn is_offered(&self, side: Side) -> bool {
        let pairs = match side {
            Side::Send => matches!(self.kind, Kind::Offer(_) | Kind::OfferLater(_)),
            Side::Recv => matches!(self.kind, Kind::Accept(_)),
        };
        pairs && self.waiter.is_available()
    }
}

/// A side of a channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Send,
    Recv,
}

struct State<T> {
    /// The execution this state belongs to (see `crate::generation`).
    generation: u64,
    /// The messages in the buffer, oldest first.
    queue: VecDeque<Message<T>>,
    /// Places in a bounded channel's buffer that selected send arms reserved, and fill when they
    /// complete.
    reserved: usize,
    /// For a bounded channel, the clocks of the receives that freed the places the next sends take,
    /// oldest first. It starts with the channel's capacity of empty clocks.
    freed: VecDeque<VectorClock>,
    senders: usize,
    receivers: usize,
    /// The clocks of the tasks that dropped the last handle of a side.
    disconnect_clock: VectorClock,
    /// Blocked operations that send, or wait until sending can go on.
    send_side: Vec<Entry<T>>,
    /// Blocked operations that receive, or wait until receiving can go on.
    recv_side: Vec<Entry<T>>,
}

/// What a state that starts over leaves behind, to be dropped once it is no longer borrowed: the
/// messages and offers in it have destructors of their own, which may use the channel.
type Leftovers<T> = (VecDeque<Message<T>>, Vec<Entry<T>>, Vec<Entry<T>>);

impl<T> State<T> {
    fn new(flavor: Flavor, generation: u64) -> Self {
        Self {
            generation,
            queue: VecDeque::new(),
            reserved: 0,
            freed: Self::initial_freed(flavor),
            senders: 1,
            receivers: 1,
            disconnect_clock: VectorClock::new(),
            send_side: Vec::new(),
            recv_side: Vec::new(),
        }
    }

    fn initial_freed(flavor: Flavor) -> VecDeque<VectorClock> {
        match flavor {
            // One clock per place in the buffer. A huge capacity is capped, since the clocks are only
            // there for causality, and an empty clock adds none: a missing one is the same.
            Flavor::Array(cap) => (0..cap.min(1024)).map(|_| VectorClock::new()).collect(),
            Flavor::List | Flavor::Zero => VecDeque::new(),
        }
    }

    /// Starts the state over for a new execution, keeping the handle counts, which belong to the
    /// handles rather than to an execution.
    fn start_over(&mut self, flavor: Flavor, generation: u64) -> Leftovers<T> {
        self.generation = generation;
        self.reserved = 0;
        self.freed = Self::initial_freed(flavor);
        self.disconnect_clock = VectorClock::new();
        (
            std::mem::take(&mut self.queue),
            std::mem::take(&mut self.send_side),
            std::mem::take(&mut self.recv_side),
        )
    }

    fn side(&self, side: Side) -> &[Entry<T>] {
        match side {
            Side::Send => &self.send_side,
            Side::Recv => &self.recv_side,
        }
    }

    fn side_mut(&mut self, side: Side) -> &mut Vec<Entry<T>> {
        match side {
            Side::Send => &mut self.send_side,
            Side::Recv => &mut self.recv_side,
        }
    }

    /// Wakes every task blocked on `side`, to try again.
    fn wake(&self, side: Side) {
        for entry in self.side(side) {
            entry.waiter.wake();
        }
    }

    /// The operation on `side` that has waited longest of those a counterpart can complete now.
    fn first_offered(&self, side: Side) -> Option<&Entry<T>> {
        self.side(side).iter().find(|entry| entry.is_offered(side))
    }

    /// The entry of `waiter`'s operation `op` on `side`.
    fn entry_of(&self, side: Side, waiter: &Rc<Waiter>, op: usize) -> Option<&Entry<T>> {
        self.side(side)
            .iter()
            .find(|entry| Rc::ptr_eq(&entry.waiter, waiter) && entry.op == op)
    }
}

/// What receiving took: a message, or a claim on a select's send arm, whose message is still to
/// arrive in the slot.
pub(crate) enum Received<T> {
    Message(Message<T>),
    Pending(Rc<Slot<T>>),
}

impl<T> Received<T> {
    /// The message, once it is there.
    pub(crate) fn wait(self) -> Message<T> {
        match self {
            Received::Message(message) => message,
            Received::Pending(slot) => slot.await_message(),
        }
    }
}

/// An unbounded, bounded or zero-capacity channel.
pub(crate) struct Chan<T> {
    flavor: Flavor,
    state: RefCell<State<T>>,
}

// Safety: a channel is only ever used by the tasks of one execution, which Shuttle runs on one
// thread, one at a time, and switches between only at scheduling points, where no borrow of the
// state is held. The bounds are crossbeam's.
unsafe impl<T: Send> Send for Chan<T> {}
unsafe impl<T: Send> Sync for Chan<T> {}

impl<T> Chan<T> {
    /// A channel with one sender and one receiver. Panics outside of a Shuttle execution.
    pub(crate) fn new(flavor: Flavor) -> Self {
        Self {
            flavor,
            state: RefCell::new(State::new(flavor, generation::current())),
        }
    }

    /// Borrows the state, starting it over first if it belongs to an earlier execution.
    fn lock(&self) -> RefMut<'_, State<T>> {
        let current = generation::current();
        let leftovers = {
            let mut state = self.state.borrow_mut();
            (state.generation != current).then(|| state.start_over(self.flavor, current))
        };
        drop(leftovers);
        self.state.borrow_mut()
    }

    /// Fails an operation because the channel is disconnected, taking causality from the tasks
    /// that disconnected it.
    fn disconnected(state: RefMut<'_, State<T>>) {
        let clock = state.disconnect_clock.clone();
        drop(state);
        join(&clock);
    }

    // ---- handles -------------------------------------------------------------------------------

    /// Counts a new handle of `side`. Not a scheduling point: no other task can tell.
    pub(crate) fn acquire(&self, side: Side) {
        let mut state = self.state.borrow_mut();
        match side {
            Side::Send => state.senders += 1,
            Side::Recv => state.receivers += 1,
        }
    }

    /// Counts a dropped handle of `side`. Dropping the last handle of a side disconnects the channel,
    /// which other tasks can tell, so it is a scheduling point; dropping any other handle is not.
    pub(crate) fn release(&self, side: Side) {
        let count = |state: &State<T>| match side {
            Side::Send => state.senders,
            Side::Recv => state.receivers,
        };
        // Outside of an execution, or once it is abandoned, there is no task to switch to or wake.
        if ExecutionState::should_stop() {
            let leftovers = {
                let mut state = self.state.borrow_mut();
                let last = self.decrement(&mut state, side);
                (last && side == Side::Recv && self.flavor == Flavor::List).then(|| std::mem::take(&mut state.queue))
            };
            drop(leftovers);
            return;
        }
        if count(&self.state.borrow()) == 1 {
            thread::switch();
        }
        let leftovers = {
            let mut state = self.lock();
            if !self.decrement(&mut state, side) {
                return;
            }
            state.disconnect_clock.update(&my_clock());
            state.wake(Side::Send);
            state.wake(Side::Recv);
            // An unbounded channel drops its messages once no receiver is left, as crossbeam's
            // does. The other channels keep them until the channel is dropped.
            (side == Side::Recv && self.flavor == Flavor::List).then(|| std::mem::take(&mut state.queue))
        };
        drop(leftovers);
    }

    /// Counts a dropped handle, and returns whether it was the last of its side.
    fn decrement(&self, state: &mut State<T>, side: Side) -> bool {
        let count = match side {
            Side::Send => &mut state.senders,
            Side::Recv => &mut state.receivers,
        };
        assert!(*count > 0, "dropped a channel handle that was never counted");
        *count -= 1;
        *count == 0
    }

    // ---- queries -------------------------------------------------------------------------------

    /// The number of messages in the buffer.
    pub(crate) fn len(&self) -> usize {
        match self.flavor {
            Flavor::Zero => 0,
            Flavor::List | Flavor::Array(_) => self.lock().queue.len(),
        }
    }

    pub(crate) fn is_full(&self) -> bool {
        match self.flavor {
            Flavor::List => false,
            Flavor::Array(cap) => {
                let state = self.lock();
                state.queue.len() + state.reserved >= cap
            }
            Flavor::Zero => true,
        }
    }

    pub(crate) fn capacity(&self) -> Option<usize> {
        match self.flavor {
            Flavor::List => None,
            Flavor::Array(cap) => Some(cap),
            Flavor::Zero => Some(0),
        }
    }

    /// Whether a send could go on now, if only to fail because the channel is disconnected.
    pub(crate) fn is_send_ready(&self) -> bool {
        let state = self.lock();
        state.receivers == 0
            || match self.flavor {
                Flavor::List => true,
                Flavor::Array(cap) => state.queue.len() + state.reserved < cap,
                Flavor::Zero => state.first_offered(Side::Recv).is_some(),
            }
    }

    /// Whether a receive could go on now, if only to fail because the channel is disconnected.
    pub(crate) fn is_recv_ready(&self) -> bool {
        let state = self.lock();
        state.senders == 0
            || match self.flavor {
                Flavor::List | Flavor::Array(_) => !state.queue.is_empty(),
                Flavor::Zero => state.first_offered(Side::Send).is_some(),
            }
    }

    // ---- operations that do not wait ------------------------------------------------------------

    /// Sends `value` if that needs no waiting. Not a scheduling point.
    pub(crate) fn send_now(&self, value: T) -> Result<(), TrySendError<T>> {
        let mut state = self.lock();
        if state.receivers == 0 {
            Self::disconnected(state);
            return Err(TrySendError::Disconnected(value));
        }
        match self.flavor {
            Flavor::List => {
                Self::push(&mut state, value);
                Ok(())
            }
            Flavor::Array(cap) => {
                if state.queue.len() + state.reserved >= cap {
                    return Err(TrySendError::Full(value));
                }
                let freed = state.freed.pop_front().unwrap_or_else(VectorClock::new);
                join(&freed);
                Self::push(&mut state, value);
                Ok(())
            }
            Flavor::Zero => {
                let Some(entry) = state.first_offered(Side::Recv) else {
                    return Err(TrySendError::Full(value));
                };
                let Kind::Accept(slot) = &entry.kind else {
                    unreachable!("only a receive is offered on the receiving side")
                };
                entry.waiter.select(entry.op);
                slot.deliver(entry.waiter.task(), value);
                Ok(())
            }
        }
    }

    /// Puts a message in the buffer, and wakes the blocked receivers.
    fn push(state: &mut State<T>, value: T) {
        let clock = send_clock();
        state.queue.push_back(Message { value, clock });
        state.wake(Side::Recv);
    }

    /// Receives a message if that needs no waiting for one to be sent. Not a scheduling point.
    ///
    /// From a zero-capacity channel whose only offer is a select's send arm, this claims the arm and
    /// returns `Received::Pending`: the select still has to work out its message.
    pub(crate) fn recv_now(&self) -> Result<Received<T>, TryRecvError> {
        let mut state = self.lock();
        match self.flavor {
            Flavor::List | Flavor::Array(_) => {
                if let Some(message) = state.queue.pop_front() {
                    if let Flavor::Array(_) = self.flavor {
                        state.freed.push_back(my_clock());
                        state.wake(Side::Send);
                    }
                    return Ok(Received::Message(message));
                }
            }
            Flavor::Zero => {
                if let Some(entry) = state.first_offered(Side::Send) {
                    entry.waiter.select(entry.op);
                    let received = match &entry.kind {
                        Kind::Offer(slot) => {
                            let message = slot.message.borrow_mut().take().expect("an offer holds its message");
                            *slot.receiver_clock.borrow_mut() = Some(my_clock());
                            Received::Message(message)
                        }
                        Kind::OfferLater(slot) => {
                            let mine = Rc::new(Slot::empty());
                            *slot.claimed_by.borrow_mut() = Some((Rc::clone(&mine), ExecutionState::me()));
                            Received::Pending(mine)
                        }
                        Kind::Retry | Kind::Accept(_) => unreachable!("only a send is offered on the sending side"),
                    };
                    entry.waiter.wake();
                    return Ok(received);
                }
            }
        }
        if state.senders == 0 {
            Self::disconnected(state);
            return Err(TryRecvError::Disconnected);
        }
        Err(TryRecvError::Empty)
    }

    // ---- blocking operations --------------------------------------------------------------------

    /// `send` and `send_deadline`: one scheduling point, and then a wait for the send to go on, until
    /// `deadline` if there is one.
    pub(crate) fn send(&self, mut value: T, deadline: Option<Instant>) -> Result<(), SendTimeoutError<T>> {
        thread::switch();
        loop {
            match self.send_now(value) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(value)) => return Err(SendTimeoutError::Disconnected(value)),
                Err(TrySendError::Full(rejected)) => value = rejected,
            }
            if deadline.is_some_and(|deadline| peek() >= deadline) {
                return Err(SendTimeoutError::Timeout(value));
            }

            let waiter = Rc::new(Waiter::new());
            // A zero-capacity send offers its message, so that a receiver can take it while this task
            // is blocked. A send into a buffer just waits for room.
            let offer = match self.flavor {
                Flavor::Zero => {
                    let slot = Rc::new(Slot::holding(Message {
                        value,
                        clock: send_clock(),
                    }));
                    self.register(Side::Send, &waiter, 0, Kind::Offer(Rc::clone(&slot)));
                    Err(slot)
                }
                Flavor::List | Flavor::Array(_) => {
                    self.register(Side::Send, &waiter, 0, Kind::Retry);
                    Ok(value)
                }
            };
            let registered = Registered {
                chan: self,
                waiter: &waiter,
            };
            waiter::block(deadline);
            drop(registered);

            value = match offer {
                Ok(value) => value,
                Err(slot) => {
                    if waiter.selected().is_some() {
                        // A receiver took the message.
                        if let Some(clock) = slot.receiver_clock.borrow_mut().take() {
                            join(&clock);
                        }
                        return Ok(());
                    }
                    slot.message
                        .borrow_mut()
                        .take()
                        .expect("an untaken offer holds its message")
                        .value
                }
            };
            // Try again, also after a timeout: something may have changed at the deadline.
        }
    }

    /// `recv` and `recv_deadline`: one scheduling point, and then a wait for a message, until
    /// `deadline` if there is one.
    pub(crate) fn recv(&self, deadline: Option<Instant>) -> Result<T, RecvTimeoutError> {
        thread::switch();
        loop {
            match self.recv_now() {
                Ok(received) => return Ok(Self::take(received)),
                Err(TryRecvError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
                Err(TryRecvError::Empty) => {}
            }
            if deadline.is_some_and(|deadline| peek() >= deadline) {
                return Err(RecvTimeoutError::Timeout);
            }

            let waiter = Rc::new(Waiter::new());
            let slot = self.register_recv(&waiter, 0);
            let registered = Registered {
                chan: self,
                waiter: &waiter,
            };
            waiter::block(deadline);
            drop(registered);

            if let (Some(_), Some(slot)) = (waiter.selected(), slot) {
                // A sender completed the receive, or claimed it and is about to.
                return Ok(Self::take(Received::Pending(slot)));
            }
        }
    }

    /// The value of a received message, taking causality from its sender.
    pub(crate) fn take(received: Received<T>) -> T {
        let message = received.wait();
        join(&message.clock);
        message.value
    }

    // ---- registration ---------------------------------------------------------------------------

    /// Registers `waiter`'s operation `op` with `side`. An operation that a counterpart can complete
    /// on a zero-capacity channel may make the other side ready, so it wakes the tasks waiting there.
    fn register(&self, side: Side, waiter: &Rc<Waiter>, op: usize, kind: Kind<T>) {
        let mut state = self.lock();
        let offered = !matches!(kind, Kind::Retry);
        state.side_mut(side).push(Entry {
            waiter: Rc::clone(waiter),
            op,
            kind,
        });
        if offered {
            let other = match side {
                Side::Send => Side::Recv,
                Side::Recv => Side::Send,
            };
            state.wake(other);
        }
    }

    /// Registers a blocked receive, and returns the slot that a zero-capacity channel's sender puts
    /// the message in.
    pub(crate) fn register_recv(&self, waiter: &Rc<Waiter>, op: usize) -> Option<Rc<Slot<T>>> {
        match self.flavor {
            Flavor::Zero => {
                let slot = Rc::new(Slot::empty());
                self.register(Side::Recv, waiter, op, Kind::Accept(Rc::clone(&slot)));
                Some(slot)
            }
            Flavor::List | Flavor::Array(_) => {
                self.register(Side::Recv, waiter, op, Kind::Retry);
                None
            }
        }
    }

    /// Registers a blocked select's send arm. A zero-capacity channel's receiver can claim it.
    pub(crate) fn register_send_arm(&self, waiter: &Rc<Waiter>, op: usize) {
        let kind = match self.flavor {
            Flavor::Zero => Kind::OfferLater(Rc::new(Slot::empty())),
            Flavor::List | Flavor::Array(_) => Kind::Retry,
        };
        self.register(Side::Send, waiter, op, kind);
    }

    /// Registers a wait for `side` to become ready, which no counterpart can complete.
    pub(crate) fn watch(&self, side: Side, waiter: &Rc<Waiter>, op: usize) {
        self.register(side, waiter, op, Kind::Retry);
    }

    /// Removes everything `waiter` registered, on both sides. Leaves the state alone otherwise, so
    /// that it works while a task unwinds and during execution teardown.
    pub(crate) fn unregister(&self, waiter: &Rc<Waiter>) {
        let removed = {
            let mut state = self.state.borrow_mut();
            let mut removed = Vec::new();
            for side in [Side::Send, Side::Recv] {
                let entries = state.side_mut(side);
                let mut i = 0;
                while i < entries.len() {
                    if Rc::ptr_eq(&entries[i].waiter, waiter) {
                        removed.push(entries.remove(i));
                    } else {
                        i += 1;
                    }
                }
            }
            removed
        };
        // An untaken offer's message is dropped here, outside the borrow.
        drop(removed);
    }

    // ---- selection ------------------------------------------------------------------------------

    /// Begins a selected send arm, if it can go on now, and records in `token` how to complete it.
    pub(crate) fn select_send(&self, token: &mut Token) -> bool {
        let mut state = self.lock();
        if state.receivers == 0 {
            Self::disconnected(state);
            *token = Token::disconnected();
            return true;
        }
        match self.flavor {
            Flavor::List => {
                *token = Token::reserved();
                true
            }
            Flavor::Array(cap) => {
                if state.queue.len() + state.reserved >= cap {
                    return false;
                }
                state.reserved += 1;
                let freed = state.freed.pop_front().unwrap_or_else(VectorClock::new);
                drop(state);
                join(&freed);
                *token = Token::reserved();
                true
            }
            Flavor::Zero => {
                let Some(entry) = state.first_offered(Side::Recv) else {
                    return false;
                };
                let Kind::Accept(slot) = &entry.kind else {
                    unreachable!("only a receive is offered on the receiving side")
                };
                // The receiver stays blocked until the message is written, which the select does
                // once it has worked out what the message is.
                entry.waiter.select(entry.op);
                *token = Token::deliver(Rc::clone(slot), entry.waiter.task());
                true
            }
        }
    }

    /// Begins a selected receive arm, if it can go on now, and records in `token` how to complete it.
    pub(crate) fn select_recv(&self, token: &mut Token) -> bool {
        match self.recv_now() {
            Ok(received) => {
                *token = Token::received(received);
                true
            }
            Err(TryRecvError::Disconnected) => {
                *token = Token::disconnected();
                true
            }
            Err(TryRecvError::Empty) => false,
        }
    }

    /// Records in `token` how to complete `waiter`'s operation `op` on `side`, which a counterpart
    /// completed or claimed while the waiter was blocked. Only zero-capacity channels do that.
    pub(crate) fn accept(&self, side: Side, token: &mut Token, waiter: &Rc<Waiter>, op: usize) {
        let state = self.state.borrow();
        let entry = state
            .entry_of(side, waiter, op)
            .expect("a selected operation is registered until its waiter accepts it");
        *token = match &entry.kind {
            Kind::Accept(slot) => Token::received::<T>(Received::Pending(Rc::clone(slot))),
            Kind::OfferLater(slot) => {
                let (receiver_slot, receiver) = slot
                    .claimed_by
                    .borrow_mut()
                    .take()
                    .expect("a claimed send arm knows its receiver");
                Token::deliver(receiver_slot, receiver)
            }
            Kind::Retry | Kind::Offer(_) => unreachable!("no counterpart selects this operation"),
        };
    }

    /// Completes a selected send arm.
    pub(crate) fn write(&self, token: Token, value: T) -> Result<(), T> {
        match token.into_send::<T>() {
            SendToken::Disconnected => Err(value),
            SendToken::Reserved => {
                let mut state = self.lock();
                if let Flavor::Array(_) = self.flavor {
                    state.reserved -= 1;
                }
                Self::push(&mut state, value);
                Ok(())
            }
            SendToken::Deliver(slot, receiver) => {
                slot.deliver(receiver, value);
                Ok(())
            }
        }
    }
}

/// Removes a blocked operation's registrations when it stops waiting, also when its task unwinds
/// from the wait during execution teardown, after which other tasks' destructors may still use
/// the channel.
struct Registered<'a, T> {
    chan: &'a Chan<T>,
    waiter: &'a Rc<Waiter>,
}

impl<T> Drop for Registered<'_, T> {
    fn drop(&mut self) {
        self.chan.unregister(self.waiter);
    }
}

/// How to complete a selected send arm.
pub(crate) enum SendToken<T> {
    Disconnected,
    /// A place in the buffer is the arm's: an unbounded channel always has one.
    Reserved,
    /// A zero-capacity channel's receiver waits for the message in this slot.
    Deliver(Rc<Slot<T>>, TaskId),
}

impl Token {
    /// What the token says about a send arm of a `Chan<T>`.
    fn into_send<T>(self) -> SendToken<T> {
        match self.0 {
            TokenKind::Disconnected => SendToken::Disconnected,
            TokenKind::Reserved => SendToken::Reserved,
            // Safety: only `Chan<T>::select_send` and `Chan<T>::accept` make a `Deliver` token, from
            // an `Rc<Slot<T>>`, and `SelectedOperation::send` checks that the sender it completes is
            // the one that was selected, so `T` is the same.
            TokenKind::Deliver(slot, receiver) => SendToken::Deliver(unsafe { slot.take::<Rc<Slot<T>>>() }, receiver),
            _ => unreachable!("a send arm was completed with a receive arm's token"),
        }
    }

    /// What the token says about a receive arm of a `Chan<T>`: what it received, or `None` if the
    /// channel is disconnected.
    pub(crate) fn into_received<T>(self) -> Option<Received<T>> {
        match self.0 {
            TokenKind::Disconnected => None,
            // Safety: only `Chan<T>::select_recv` and `Chan<T>::accept` make a `Received` token, from
            // a `Received<T>`, and `SelectedOperation::recv` checks that the receiver it completes
            // is the one that was selected, so `T` is the same.
            TokenKind::Received(received) => Some(unsafe { received.take::<Received<T>>() }),
            _ => unreachable!("a receive arm was completed with a send arm's token"),
        }
    }
}

/// When a wait has to end: at `deadline`, if there is one, or at the earliest of the timers it
/// waits on.
pub(crate) fn earliest(deadline: Option<Instant>, timers: impl Iterator<Item = Instant>) -> Option<Instant> {
    timers.fold(deadline, |earliest, timer| {
        Some(earliest.map_or(timer, |earliest| earliest.min(timer)))
    })
}
