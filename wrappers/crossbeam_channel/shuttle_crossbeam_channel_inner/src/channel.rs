//! The channel interface: crossbeam-channel's constructors, `Sender`, `Receiver` and iterators.

use crate::chan::{Chan, Flavor, Side};
use crate::select::{SelectHandle, Token};
use crate::timer::{At, Tick};
use crate::waiter::{self, Waiter};
use crossbeam_channel::{RecvError, RecvTimeoutError, SendError, SendTimeoutError, TryRecvError, TrySendError};
use shuttle_engine::runtime::thread;
use shuttle_time_inner::time::{Duration, Instant};
use shuttle_time_inner::timer_support::peek;
use std::fmt;
use std::iter::FusedIterator;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::rc::Rc;
use std::sync::Arc;

/// Creates a channel of unbounded capacity.
///
/// This channel has a growable buffer that can hold any number of messages at a time.
///
/// Must be called inside a Shuttle test, as must every operation on the channel.
pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    let chan = Arc::new(Chan::new(Flavor::List));
    (
        Sender {
            chan: Arc::clone(&chan),
        },
        Receiver {
            flavor: ReceiverFlavor::Chan(chan),
        },
    )
}

/// Creates a channel of bounded capacity.
///
/// This channel has a buffer that can hold at most `cap` messages at a time.
///
/// A special case is zero-capacity channel, which cannot hold any messages. Instead, send and
/// receive operations must appear at the same time in order to pair up and pass the message over.
///
/// Must be called inside a Shuttle test, as must every operation on the channel.
pub fn bounded<T>(cap: usize) -> (Sender<T>, Receiver<T>) {
    let flavor = if cap == 0 { Flavor::Zero } else { Flavor::Array(cap) };
    let chan = Arc::new(Chan::new(flavor));
    (
        Sender {
            chan: Arc::clone(&chan),
        },
        Receiver {
            flavor: ReceiverFlavor::Chan(chan),
        },
    )
}

/// Creates a receiver that delivers a message after a certain duration of time.
///
/// The channel is bounded with capacity of 1 and never gets disconnected. Exactly one message will
/// be sent into the channel after `duration` elapses. The message is the instant at which it is
/// sent.
///
/// The duration is measured on `shuttle-time`'s clock, which moves to the delivery time only when
/// a task waits for the message and no task can run (see the [crate documentation](crate#time)).
pub fn after(duration: Duration) -> Receiver<Instant> {
    match peek().checked_add(duration) {
        Some(deadline) => at(deadline),
        None => never(),
    }
}

/// Creates a receiver that delivers a message at a certain instant in time.
///
/// The channel is bounded with capacity of 1 and never gets disconnected. Exactly one message will
/// be sent into the channel at the moment in time `when`. The message is the instant at which it
/// is sent, which is the same as `when`. If `when` is in the past, the message will be delivered
/// instantly to the receiver.
pub fn at(when: Instant) -> Receiver<Instant> {
    Receiver {
        flavor: ReceiverFlavor::At(Arc::new(At::new(when)), std::convert::identity),
    }
}

/// Creates a receiver that never delivers messages.
///
/// The channel is bounded with capacity of 0 and never gets disconnected.
pub const fn never<T>() -> Receiver<T> {
    Receiver {
        flavor: ReceiverFlavor::Never,
    }
}

/// Creates a receiver that delivers messages periodically.
///
/// The channel is bounded with capacity of 1 and never gets disconnected. Messages will be
/// sent into the channel in intervals of `duration`. Each message is the instant at which it is
/// sent.
pub fn tick(duration: Duration) -> Receiver<Instant> {
    match peek().checked_add(duration) {
        Some(first) => Receiver {
            flavor: ReceiverFlavor::Tick(Arc::new(Tick::new(first, duration)), std::convert::identity),
        },
        None => never(),
    }
}

/// The sending side of a channel.
pub struct Sender<T> {
    chan: Arc<Chan<T>>,
}

// The bounds are crossbeam's.
unsafe impl<T: Send> Send for Sender<T> {}
unsafe impl<T: Send> Sync for Sender<T> {}

impl<T> UnwindSafe for Sender<T> {}
impl<T> RefUnwindSafe for Sender<T> {}

impl<T> Sender<T> {
    /// Attempts to send a message into the channel without blocking.
    ///
    /// This method will either send a message into the channel immediately or return an error if
    /// the channel is full or disconnected. The returned error contains the original message.
    ///
    /// If called on a zero-capacity channel, this method will send the message only if there
    /// happens to be a receive operation on the other side of the channel at the same time.
    ///
    /// A scheduling point.
    pub fn try_send(&self, msg: T) -> Result<(), TrySendError<T>> {
        thread::switch();
        self.chan.send_now(msg)
    }

    /// Blocks the current thread until a message is sent or the channel is disconnected.
    ///
    /// If the channel is full and not disconnected, this call will block until the send operation
    /// can proceed. If the channel becomes disconnected, this call will wake up and return an
    /// error. The returned error contains the original message.
    ///
    /// If called on a zero-capacity channel, this method will wait for a receive operation to
    /// appear on the other side of the channel.
    ///
    /// A scheduling point, and a blocking one if the send has to wait.
    pub fn send(&self, msg: T) -> Result<(), SendError<T>> {
        self.chan.send(msg, None).map_err(|err| match err {
            SendTimeoutError::Disconnected(msg) => SendError(msg),
            SendTimeoutError::Timeout(_) => unreachable!("a send without a deadline does not time out"),
        })
    }

    /// Waits for a message to be sent into the channel, but only for a limited time.
    ///
    /// The time is measured on `shuttle-time`'s clock: the send times out only when no task can
    /// run (see the [crate documentation](crate#time)).
    pub fn send_timeout(&self, msg: T, timeout: Duration) -> Result<(), SendTimeoutError<T>> {
        match peek().checked_add(timeout) {
            Some(deadline) => self.send_deadline(msg, deadline),
            None => self.send(msg).map_err(SendTimeoutError::from),
        }
    }

    /// Waits for a message to be sent into the channel, but only until a given deadline.
    ///
    /// The deadline is an instant of `shuttle-time`'s clock: the send times out only when no task
    /// can run (see the [crate documentation](crate#time)).
    pub fn send_deadline(&self, msg: T, deadline: Instant) -> Result<(), SendTimeoutError<T>> {
        self.chan.send(msg, Some(deadline))
    }

    /// Returns `true` if the channel is empty. Zero-capacity channels are always empty.
    ///
    /// A scheduling point.
    pub fn is_empty(&self) -> bool {
        thread::switch();
        self.chan.len() == 0
    }

    /// Returns `true` if the channel is full. Zero-capacity channels are always full.
    ///
    /// A scheduling point.
    pub fn is_full(&self) -> bool {
        thread::switch();
        self.chan.is_full()
    }

    /// Returns the number of messages in the channel.
    ///
    /// A scheduling point.
    pub fn len(&self) -> usize {
        thread::switch();
        self.chan.len()
    }

    /// If the channel is bounded, returns its capacity. Not a scheduling point: it never changes.
    pub fn capacity(&self) -> Option<usize> {
        self.chan.capacity()
    }

    /// Returns `true` if senders belong to the same channel.
    pub fn same_channel(&self, other: &Sender<T>) -> bool {
        Arc::ptr_eq(&self.chan, &other.chan)
    }

    /// Completes a selected send arm.
    pub(crate) fn write(&self, token: Token, msg: T) -> Result<(), T> {
        self.chan.write(token, msg)
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.chan.release(Side::Send);
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.chan.acquire(Side::Send);
        Sender {
            chan: Arc::clone(&self.chan),
        }
    }
}

impl<T> fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Sender { .. }")
    }
}

/// The receiving side of a channel.
pub struct Receiver<T> {
    flavor: ReceiverFlavor<T>,
}

enum ReceiverFlavor<T> {
    Chan(Arc<Chan<T>>),
    /// `after` and `at`. Only a `Receiver<Instant>` has this flavor, so the function is the identity;
    /// it lets the code here be generic over `T` without converting an `Instant` unsafely.
    At(Arc<At>, fn(Instant) -> T),
    /// `tick`, likewise.
    Tick(Arc<Tick>, fn(Instant) -> T),
    Never,
}

// The bounds are crossbeam's.
unsafe impl<T: Send> Send for Receiver<T> {}
unsafe impl<T: Send> Sync for Receiver<T> {}

impl<T> UnwindSafe for Receiver<T> {}
impl<T> RefUnwindSafe for Receiver<T> {}

impl<T> Receiver<T> {
    /// Attempts to receive a message from the channel without blocking.
    ///
    /// This method will either receive a message from the channel immediately or return an error
    /// if the channel is empty.
    ///
    /// If called on a zero-capacity channel, this method will receive a message only if there
    /// happens to be a send operation on the other side of the channel at the same time.
    ///
    /// A scheduling point.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        thread::switch();
        match &self.flavor {
            ReceiverFlavor::Chan(chan) => chan.recv_now().map(Chan::take),
            ReceiverFlavor::At(at, convert) => at.try_recv().map(convert).ok_or(TryRecvError::Empty),
            ReceiverFlavor::Tick(tick, convert) => tick.try_recv().map(convert).ok_or(TryRecvError::Empty),
            ReceiverFlavor::Never => Err(TryRecvError::Empty),
        }
    }

    /// Blocks the current thread until a message is received or the channel is empty and
    /// disconnected.
    ///
    /// If the channel is empty and not disconnected, this call will block until the receive
    /// operation can proceed. If the channel is empty and becomes disconnected, this call will
    /// wake up and return an error.
    ///
    /// If called on a zero-capacity channel, this method will wait for a send operation to appear
    /// on the other side of the channel.
    ///
    /// A scheduling point, and a blocking one if the receive has to wait.
    pub fn recv(&self) -> Result<T, RecvError> {
        self.recv_until(None).map_err(|_| RecvError)
    }

    /// Waits for a message to be received from the channel, but only for a limited time.
    ///
    /// The time is measured on `shuttle-time`'s clock: the receive times out only when no task can
    /// run (see the [crate documentation](crate#time)).
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        match peek().checked_add(timeout) {
            Some(deadline) => self.recv_deadline(deadline),
            None => self.recv().map_err(RecvTimeoutError::from),
        }
    }

    /// Waits for a message to be received from the channel, but only before a given deadline.
    ///
    /// The deadline is an instant of `shuttle-time`'s clock: the receive times out only when no
    /// task can run (see the [crate documentation](crate#time)).
    pub fn recv_deadline(&self, deadline: Instant) -> Result<T, RecvTimeoutError> {
        self.recv_until(Some(deadline))
    }

    fn recv_until(&self, deadline: Option<Instant>) -> Result<T, RecvTimeoutError> {
        match &self.flavor {
            ReceiverFlavor::Chan(chan) => chan.recv(deadline),
            ReceiverFlavor::At(at, convert) => recv_timer(|| at.try_recv(), || at.deadline(), deadline).map(convert),
            ReceiverFlavor::Tick(tick, convert) => {
                recv_timer(|| tick.try_recv(), || tick.deadline(), deadline).map(convert)
            }
            ReceiverFlavor::Never => {
                recv_timer(|| None, || None, deadline).map(|_| unreachable!("`never` delivers no message"))
            }
        }
    }

    /// Returns `true` if the channel is empty.
    ///
    /// A scheduling point.
    pub fn is_empty(&self) -> bool {
        thread::switch();
        self.len_now() == 0
    }

    /// Returns `true` if the channel is full.
    ///
    /// A scheduling point.
    pub fn is_full(&self) -> bool {
        thread::switch();
        match &self.flavor {
            ReceiverFlavor::Chan(chan) => chan.is_full(),
            ReceiverFlavor::At(..) | ReceiverFlavor::Tick(..) => self.len_now() == 1,
            ReceiverFlavor::Never => true,
        }
    }

    /// Returns the number of messages in the channel.
    ///
    /// A scheduling point.
    pub fn len(&self) -> usize {
        thread::switch();
        self.len_now()
    }

    fn len_now(&self) -> usize {
        match &self.flavor {
            ReceiverFlavor::Chan(chan) => chan.len(),
            ReceiverFlavor::At(at, _) => usize::from(at.is_ready()),
            ReceiverFlavor::Tick(tick, _) => usize::from(tick.is_ready()),
            ReceiverFlavor::Never => 0,
        }
    }

    /// If the channel is bounded, returns its capacity. Not a scheduling point: it never changes.
    pub fn capacity(&self) -> Option<usize> {
        match &self.flavor {
            ReceiverFlavor::Chan(chan) => chan.capacity(),
            ReceiverFlavor::At(..) | ReceiverFlavor::Tick(..) => Some(1),
            ReceiverFlavor::Never => Some(0),
        }
    }

    /// A blocking iterator over messages in the channel.
    ///
    /// Each call to `next` blocks waiting for the next message and then returns it. However, if
    /// the channel becomes empty and disconnected, it returns `None` without blocking.
    pub fn iter(&self) -> Iter<'_, T> {
        Iter { receiver: self }
    }

    /// A non-blocking iterator over messages in the channel.
    ///
    /// Each call to `next` returns a message if there is one ready to be received. The iterator
    /// never blocks waiting for the next message.
    pub fn try_iter(&self) -> TryIter<'_, T> {
        TryIter { receiver: self }
    }

    /// Returns `true` if receivers belong to the same channel.
    pub fn same_channel(&self, other: &Receiver<T>) -> bool {
        match (&self.flavor, &other.flavor) {
            (ReceiverFlavor::Chan(a), ReceiverFlavor::Chan(b)) => Arc::ptr_eq(a, b),
            (ReceiverFlavor::At(a, _), ReceiverFlavor::At(b, _)) => Arc::ptr_eq(a, b),
            (ReceiverFlavor::Tick(a, _), ReceiverFlavor::Tick(b, _)) => Arc::ptr_eq(a, b),
            (ReceiverFlavor::Never, ReceiverFlavor::Never) => true,
            _ => false,
        }
    }

    /// Completes a selected receive arm.
    pub(crate) fn read(&self, token: Token) -> Result<T, ()> {
        match &self.flavor {
            ReceiverFlavor::Chan(_) => token.into_received::<T>().map(Chan::take).ok_or(()),
            ReceiverFlavor::At(_, convert) | ReceiverFlavor::Tick(_, convert) => {
                token.into_timer().map(convert).ok_or(())
            }
            ReceiverFlavor::Never => unreachable!("a `never` receiver is never selected"),
        }
    }
}

/// The receive of a timer, or of `never`: one scheduling point, and then a wait until the timer is
/// due (`due` returns its delivery time) or `deadline` passes, whichever is first. Without either,
/// the wait never ends.
fn recv_timer(
    try_recv: impl Fn() -> Option<Instant>,
    due: impl Fn() -> Option<Instant>,
    deadline: Option<Instant>,
) -> Result<Instant, RecvTimeoutError> {
    thread::switch();
    loop {
        if let Some(instant) = try_recv() {
            return Ok(instant);
        }
        if deadline.is_some_and(|deadline| peek() >= deadline) {
            return Err(RecvTimeoutError::Timeout);
        }
        // Nothing but time can make a timer ready, so the wait registers nowhere.
        waiter::block(crate::chan::earliest(deadline, due().into_iter()));
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        if let ReceiverFlavor::Chan(chan) = &self.flavor {
            chan.release(Side::Recv);
        }
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        let flavor = match &self.flavor {
            ReceiverFlavor::Chan(chan) => {
                chan.acquire(Side::Recv);
                ReceiverFlavor::Chan(Arc::clone(chan))
            }
            ReceiverFlavor::At(at, convert) => ReceiverFlavor::At(Arc::clone(at), *convert),
            ReceiverFlavor::Tick(tick, convert) => ReceiverFlavor::Tick(Arc::clone(tick), *convert),
            ReceiverFlavor::Never => ReceiverFlavor::Never,
        };
        Receiver { flavor }
    }
}

impl<T> fmt::Debug for Receiver<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Receiver { .. }")
    }
}

impl<'a, T> IntoIterator for &'a Receiver<T> {
    type Item = T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T> IntoIterator for Receiver<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        IntoIter { receiver: self }
    }
}

/// A blocking iterator over messages in a channel.
///
/// Each call to [`next`] blocks waiting for the next message and then returns it. However, if the
/// channel becomes empty and disconnected, it returns [`None`] without blocking.
///
/// [`next`]: Iterator::next
pub struct Iter<'a, T> {
    receiver: &'a Receiver<T>,
}

impl<T> FusedIterator for Iter<'_, T> {}

impl<T> Iterator for Iter<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.receiver.recv().ok()
    }
}

impl<T> fmt::Debug for Iter<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Iter { .. }")
    }
}

/// A non-blocking iterator over messages in a channel.
///
/// Each call to [`next`] returns a message if there is one ready to be received. The iterator
/// never blocks waiting for the next message.
///
/// [`next`]: Iterator::next
pub struct TryIter<'a, T> {
    receiver: &'a Receiver<T>,
}

impl<T> Iterator for TryIter<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.receiver.try_recv().ok()
    }
}

impl<T> fmt::Debug for TryIter<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("TryIter { .. }")
    }
}

/// A blocking iterator over messages in a channel.
///
/// Each call to [`next`] blocks waiting for the next message and then returns it. However, if the
/// channel becomes empty and disconnected, it returns [`None`] without blocking.
///
/// [`next`]: Iterator::next
pub struct IntoIter<T> {
    receiver: Receiver<T>,
}

impl<T> FusedIterator for IntoIter<T> {}

impl<T> Iterator for IntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.receiver.recv().ok()
    }
}

impl<T> fmt::Debug for IntoIter<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("IntoIter { .. }")
    }
}

impl<T> SelectHandle for Sender<T> {
    fn is_ready(&self) -> bool {
        self.chan.is_send_ready()
    }

    fn try_select(&self, token: &mut Token) -> bool {
        self.chan.select_send(token)
    }

    fn deadline(&self) -> Option<Instant> {
        None
    }

    fn register(&self, waiter: &Rc<Waiter>, op: usize) {
        self.chan.register_send_arm(waiter, op);
    }

    fn unregister(&self, waiter: &Rc<Waiter>) {
        self.chan.unregister(waiter);
    }

    fn accept(&self, token: &mut Token, waiter: &Rc<Waiter>, op: usize) {
        self.chan.accept(Side::Send, token, waiter, op);
    }

    fn watch(&self, waiter: &Rc<Waiter>, op: usize) {
        self.chan.watch(Side::Send, waiter, op);
    }
}

impl<T> SelectHandle for Receiver<T> {
    fn is_ready(&self) -> bool {
        match &self.flavor {
            ReceiverFlavor::Chan(chan) => chan.is_recv_ready(),
            ReceiverFlavor::At(at, _) => at.is_ready(),
            ReceiverFlavor::Tick(tick, _) => tick.is_ready(),
            ReceiverFlavor::Never => false,
        }
    }

    fn try_select(&self, token: &mut Token) -> bool {
        let instant = match &self.flavor {
            ReceiverFlavor::Chan(chan) => return chan.select_recv(token),
            ReceiverFlavor::At(at, _) => at.try_recv(),
            ReceiverFlavor::Tick(tick, _) => tick.try_recv(),
            ReceiverFlavor::Never => None,
        };
        match instant {
            Some(instant) => {
                *token = Token::timer(instant);
                true
            }
            None => false,
        }
    }

    fn deadline(&self) -> Option<Instant> {
        match &self.flavor {
            ReceiverFlavor::Chan(_) | ReceiverFlavor::Never => None,
            ReceiverFlavor::At(at, _) => at.deadline(),
            ReceiverFlavor::Tick(tick, _) => tick.deadline(),
        }
    }

    fn register(&self, waiter: &Rc<Waiter>, op: usize) {
        // A timer needs no registration: the wait's deadline is the timer's (see `deadline`).
        if let ReceiverFlavor::Chan(chan) = &self.flavor {
            chan.register_recv(waiter, op);
        }
    }

    fn unregister(&self, waiter: &Rc<Waiter>) {
        if let ReceiverFlavor::Chan(chan) = &self.flavor {
            chan.unregister(waiter);
        }
    }

    fn accept(&self, token: &mut Token, waiter: &Rc<Waiter>, op: usize) {
        match &self.flavor {
            ReceiverFlavor::Chan(chan) => chan.accept(Side::Recv, token, waiter, op),
            _ => unreachable!("no counterpart selects a timer's operation"),
        }
    }

    fn watch(&self, waiter: &Rc<Waiter>, op: usize) {
        if let ReceiverFlavor::Chan(chan) = &self.flavor {
            chan.watch(Side::Recv, waiter, op);
        }
    }
}
