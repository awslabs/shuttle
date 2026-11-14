//! Selection: `Select`, `SelectedOperation`, and what the `select!` macro calls.
//!
//! A selection takes one scheduling point, and then picks among the operations that are ready: the
//! first of them in a biased selection, and otherwise one drawn from Shuttle's random number
//! generator (`shuttle::rand`), which records the draw in the schedule, so that a failing schedule
//! replays. It draws only when more than one operation is ready. If none is, the selection blocks
//! until one is, or until its deadline or the earliest of its timers passes, which happens only
//! when no task can run (see the [crate documentation](crate#time)).

use crate::chan::{Received, earliest};
use crate::channel::{Receiver, Sender};
use crate::erased::Erased;
use crate::waiter::{self, Waiter};
use crossbeam_channel::{ReadyTimeoutError, RecvError, SelectTimeoutError, SendError, TryReadyError, TrySelectError};
use shuttle::rand::{Rng, thread_rng};
use shuttle_engine::runtime::task::TaskId;
use shuttle_engine::runtime::thread;
use shuttle_time_inner::time::{Duration, Instant};
use shuttle_time_inner::timer_support::peek;
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

/// What a selected operation needs to complete: what a receive took, or where a send goes.
///
/// This is a private API that the `select!` macro uses.
#[doc(hidden)]
#[derive(Default)]
pub struct Token(pub(crate) TokenKind);

#[derive(Default)]
pub(crate) enum TokenKind {
    #[default]
    Empty,
    /// The operation fails, because the channel is disconnected.
    Disconnected,
    /// A send arm owns a place in a buffer.
    Reserved,
    /// A receive arm took this message, or claimed a send arm whose message is still to come: a
    /// `crate::chan::Received<T>`.
    Received(Erased),
    /// A send arm puts its message in this receiver's slot: a `Rc<crate::chan::Slot<T>>`.
    Deliver(Erased, TaskId),
    /// A timer delivered this instant.
    Timer(Instant),
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Token { .. }")
    }
}

impl Token {
    pub(crate) fn disconnected() -> Self {
        Token(TokenKind::Disconnected)
    }

    pub(crate) fn timer(instant: Instant) -> Self {
        Token(TokenKind::Timer(instant))
    }

    pub(crate) fn into_timer(self) -> Option<Instant> {
        match self.0 {
            TokenKind::Timer(instant) => Some(instant),
            TokenKind::Disconnected => None,
            _ => unreachable!("a timer was completed with a channel's token"),
        }
    }

    pub(crate) fn reserved() -> Self {
        Token(TokenKind::Reserved)
    }

    pub(crate) fn deliver<T>(slot: Rc<crate::chan::Slot<T>>, receiver: TaskId) -> Self {
        Token(TokenKind::Deliver(Erased::new(slot), receiver))
    }

    pub(crate) fn received<T>(received: Received<T>) -> Self {
        Token(TokenKind::Received(Erased::new(received)))
    }
}

/// A receiver or a sender that can take part in a selection.
///
/// This is a private API that the `select!` macro uses.
#[doc(hidden)]
pub trait SelectHandle {
    /// Whether the operation can go on without waiting, if only to fail because the channel is
    /// disconnected.
    fn is_ready(&self) -> bool;

    /// Begins the operation if it can go on without waiting, and records in `token` how to complete
    /// it. Returns whether it began.
    fn try_select(&self, token: &mut Token) -> bool;

    /// When the operation becomes ready by itself: a timer's delivery time.
    fn deadline(&self) -> Option<Instant>;

    /// Registers `waiter`'s operation `op`, so that a change of the channel wakes the waiter, and,
    /// on a zero-capacity channel, so that a counterpart can complete it.
    fn register(&self, waiter: &Rc<Waiter>, op: usize);

    /// Removes what `register` and `watch` registered for `waiter`.
    fn unregister(&self, waiter: &Rc<Waiter>);

    /// Records in `token` how to complete `waiter`'s operation `op`, which a counterpart completed or
    /// claimed while the waiter was blocked.
    fn accept(&self, token: &mut Token, waiter: &Rc<Waiter>, op: usize);

    /// Registers `waiter`'s operation `op` to wake the waiter when the operation may have become
    /// ready, without offering it to a counterpart.
    fn watch(&self, waiter: &Rc<Waiter>, op: usize);
}

impl<T: SelectHandle> SelectHandle for &T {
    fn is_ready(&self) -> bool {
        (**self).is_ready()
    }

    fn try_select(&self, token: &mut Token) -> bool {
        (**self).try_select(token)
    }

    fn deadline(&self) -> Option<Instant> {
        (**self).deadline()
    }

    fn register(&self, waiter: &Rc<Waiter>, op: usize) {
        (**self).register(waiter, op)
    }

    fn unregister(&self, waiter: &Rc<Waiter>) {
        (**self).unregister(waiter)
    }

    fn accept(&self, token: &mut Token, waiter: &Rc<Waiter>, op: usize) {
        (**self).accept(token, waiter, op)
    }

    fn watch(&self, waiter: &Rc<Waiter>, op: usize) {
        (**self).watch(waiter, op)
    }
}

/// When a selection gives up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Timeout {
    /// Without waiting.
    Now,
    /// Never.
    Never,
    /// At this instant of `shuttle-time`'s clock.
    At(Instant),
}

type Handles<'a, 'h> = &'a mut [(&'h dyn SelectHandle, usize, *const u8)];

/// One of the ready operations, by position in `handles`: the first in a biased selection, and
/// otherwise one drawn from Shuttle's random number generator, if more than one is ready.
fn choose_ready(handles: &[(&dyn SelectHandle, usize, *const u8)], is_biased: bool) -> Option<usize> {
    if is_biased {
        return handles.iter().position(|(handle, _, _)| handle.is_ready());
    }
    let ready = handles
        .iter()
        .enumerate()
        .filter(|(_, (handle, _, _))| handle.is_ready())
        .map(|(position, _)| position)
        .collect::<Vec<_>>();
    match ready.len() {
        0 => None,
        1 => Some(ready[0]),
        n => Some(ready[thread_rng().gen_range(0..n)]),
    }
}

/// Registers a waiter with every operation of a selection, and removes the registrations when it
/// stops waiting, also when its task unwinds from the wait during execution teardown.
struct Registered<'a, 'h> {
    handles: &'a [(&'h dyn SelectHandle, usize, *const u8)],
    waiter: Rc<Waiter>,
}

impl<'a, 'h> Registered<'a, 'h> {
    fn new(handles: &'a [(&'h dyn SelectHandle, usize, *const u8)], watch: bool) -> Self {
        let waiter = Rc::new(Waiter::new());
        for (op, (handle, _, _)) in handles.iter().enumerate() {
            if watch {
                handle.watch(&waiter, op);
            } else {
                handle.register(&waiter, op);
            }
        }
        Self { handles, waiter }
    }

    /// When the wait ends by itself: at `timeout`, or at the earliest timer of the selection.
    fn deadline(&self, timeout: Option<Instant>) -> Option<Instant> {
        earliest(
            timeout,
            self.handles.iter().filter_map(|(handle, _, _)| handle.deadline()),
        )
    }
}

impl Drop for Registered<'_, '_> {
    fn drop(&mut self) {
        for (handle, _, _) in self.handles {
            handle.unregister(&self.waiter);
        }
    }
}

/// A wait for nothing at all, until `timeout`.
fn sleep_until(timeout: Timeout) {
    match timeout {
        Timeout::Now => {}
        Timeout::Never => loop {
            waiter::block(None);
        },
        Timeout::At(deadline) => {
            while peek() < deadline {
                waiter::block(Some(deadline));
            }
        }
    }
}

/// Selects one of the operations, waiting until one is ready or `timeout` passes. Returns what
/// completing it takes, its index and the address of its handle.
fn run_select(handles: Handles<'_, '_>, timeout: Timeout, is_biased: bool) -> Option<(Token, usize, *const u8)> {
    thread::switch();
    if handles.is_empty() {
        sleep_until(timeout);
        return None;
    }
    loop {
        if let Some(position) = choose_ready(handles, is_biased) {
            let (handle, index, ptr) = handles[position];
            let mut token = Token::default();
            if handle.try_select(&mut token) {
                return Some((token, index, ptr));
            }
            unreachable!("a ready operation could not begin");
        }
        let timeout = match timeout {
            Timeout::Now => return None,
            Timeout::Never => None,
            Timeout::At(deadline) if peek() >= deadline => return None,
            Timeout::At(deadline) => Some(deadline),
        };

        let registered = Registered::new(handles, false);
        waiter::block(registered.deadline(timeout));
        if let Some(position) = registered.waiter.selected() {
            // A counterpart on a zero-capacity channel completed or claimed this operation.
            let (handle, index, ptr) = handles[position];
            let mut token = Token::default();
            handle.accept(&mut token, &registered.waiter, position);
            drop(registered);
            return Some((token, index, ptr));
        }
        drop(registered);
        // Try again: an operation may be ready, a timer due, or the deadline passed.
    }
}

/// Waits until one of the operations is ready, or `timeout` passes. Returns the operation's index.
fn run_ready(handles: Handles<'_, '_>, timeout: Timeout, is_biased: bool) -> Option<usize> {
    thread::switch();
    if handles.is_empty() {
        sleep_until(timeout);
        return None;
    }
    loop {
        if let Some(position) = choose_ready(handles, is_biased) {
            return Some(handles[position].1);
        }
        let timeout = match timeout {
            Timeout::Now => return None,
            Timeout::Never => None,
            Timeout::At(deadline) if peek() >= deadline => return None,
            Timeout::At(deadline) => Some(deadline),
        };
        let registered = Registered::new(handles, true);
        waiter::block(registered.deadline(timeout));
        drop(registered);
    }
}

/// Attempts to select one of the operations without blocking.
///
/// This is a private API that the `select!` macro uses.
#[doc(hidden)]
pub fn try_select<'a>(
    handles: &mut [(&'a dyn SelectHandle, usize, *const u8)],
    is_biased: bool,
) -> Result<SelectedOperation<'a>, TrySelectError> {
    match run_select(handles, Timeout::Now, is_biased) {
        None => Err(TrySelectError),
        Some((token, index, ptr)) => Ok(SelectedOperation::new(token, index, ptr)),
    }
}

/// Blocks until one of the operations becomes ready and selects it.
///
/// This is a private API that the `select!` macro uses.
#[doc(hidden)]
pub fn select<'a>(handles: &mut [(&'a dyn SelectHandle, usize, *const u8)], is_biased: bool) -> SelectedOperation<'a> {
    if handles.is_empty() {
        panic!("no operations have been added to `Select`");
    }
    let (token, index, ptr) =
        run_select(handles, Timeout::Never, is_biased).expect("a selection without a timeout selects");
    SelectedOperation::new(token, index, ptr)
}

/// Blocks for a limited time until one of the operations becomes ready and selects it.
///
/// This is a private API that the `select!` macro uses.
#[doc(hidden)]
pub fn select_timeout<'a>(
    handles: &mut [(&'a dyn SelectHandle, usize, *const u8)],
    timeout: Duration,
    is_biased: bool,
) -> Result<SelectedOperation<'a>, SelectTimeoutError> {
    match peek().checked_add(timeout) {
        Some(deadline) => select_deadline(handles, deadline, is_biased),
        None => Ok(select(handles, is_biased)),
    }
}

/// Blocks until a given deadline, or until one of the operations becomes ready and selects it.
fn select_deadline<'a>(
    handles: &mut [(&'a dyn SelectHandle, usize, *const u8)],
    deadline: Instant,
    is_biased: bool,
) -> Result<SelectedOperation<'a>, SelectTimeoutError> {
    match run_select(handles, Timeout::At(deadline), is_biased) {
        None => Err(SelectTimeoutError),
        Some((token, index, ptr)) => Ok(SelectedOperation::new(token, index, ptr)),
    }
}

/// Selects from a set of channel operations.
///
/// `Select` allows you to define a set of channel operations, wait until any one of them becomes
/// ready, and finally execute it. If multiple operations are ready at the same time, a random one
/// among them is selected, drawn from Shuttle's random number generator.
///
/// An operation is considered to be ready if it doesn't have to block. Note that it is ready even
/// when it will simply return an error because the channel is disconnected.
///
/// Once a list of operations has been built with `Select`, there are two different ways of
/// proceeding:
///
/// * Select an operation with [`try_select`](Select::try_select), [`select`](Select::select), or
///   [`select_timeout`](Select::select_timeout). If successful, the returned selected operation
///   has already begun and **must** be completed. If we don't complete it, a panic will occur.
///
/// * Wait for an operation to become ready with [`try_ready`](Select::try_ready),
///   [`ready`](Select::ready), or [`ready_timeout`](Select::ready_timeout). If successful, we may
///   attempt to execute the operation, but are not obliged to. It is possible for another thread
///   to make the operation not ready just before we try executing it, so it's wise to use a retry
///   loop.
pub struct Select<'a> {
    /// A list of senders and receivers participating in selection.
    handles: Vec<(&'a dyn SelectHandle, usize, *const u8)>,
    /// The next index to assign to an operation.
    next_index: usize,
    /// Whether to always prefer the first ready operation.
    biased: bool,
}

// The bounds are crossbeam's: a `Select` is a list of references to senders and receivers.
unsafe impl Send for Select<'_> {}
unsafe impl Sync for Select<'_> {}

impl<'a> Select<'a> {
    /// Creates an empty list of channel operations for selection.
    pub fn new() -> Select<'a> {
        Select {
            handles: Vec::with_capacity(4),
            next_index: 0,
            biased: false,
        }
    }

    /// Creates an empty list of channel operations with biased selection.
    ///
    /// When multiple handles are ready, this will select the operation nearest to the front of the
    /// list.
    pub fn new_biased() -> Self {
        Self {
            biased: true,
            ..Default::default()
        }
    }

    /// Adds a send operation. Returns the index of the added operation.
    pub fn send<T>(&mut self, s: &'a Sender<T>) -> usize {
        let i = self.next_index;
        let ptr = s as *const Sender<_> as *const u8;
        self.handles.push((s, i, ptr));
        self.next_index += 1;
        i
    }

    /// Adds a receive operation. Returns the index of the added operation.
    pub fn recv<T>(&mut self, r: &'a Receiver<T>) -> usize {
        let i = self.next_index;
        let ptr = r as *const Receiver<_> as *const u8;
        self.handles.push((r, i, ptr));
        self.next_index += 1;
        i
    }

    /// Removes a previously added operation.
    ///
    /// This is useful when an operation is selected because the channel got disconnected and we
    /// want to try again to select a different operation instead.
    ///
    /// If new operations are added after removing some, the indices of removed operations will not
    /// be reused.
    ///
    /// # Panics
    ///
    /// An attempt to remove a non-existing or already removed operation will panic.
    pub fn remove(&mut self, index: usize) {
        assert!(
            index < self.next_index,
            "index out of bounds; {} >= {}",
            index,
            self.next_index,
        );

        let i = self
            .handles
            .iter()
            .enumerate()
            .find(|(_, (_, i, _))| *i == index)
            .expect("no operation with this index")
            .0;

        self.handles.swap_remove(i);
    }

    /// Attempts to select one of the operations without blocking.
    ///
    /// If an operation is ready, it is selected and returned. If multiple operations are ready at
    /// the same time, a random one among them is selected. If none of the operations are ready,
    /// an error is returned.
    ///
    /// An operation is considered to be ready if it doesn't have to block. Note that it is ready
    /// even when it will simply return an error because the channel is disconnected.
    ///
    /// The selected operation must be completed with [`SelectedOperation::send`]
    /// or [`SelectedOperation::recv`].
    pub fn try_select(&mut self) -> Result<SelectedOperation<'a>, TrySelectError> {
        try_select(&mut self.handles, self.biased)
    }

    /// Blocks until one of the operations becomes ready and selects it.
    ///
    /// Once an operation becomes ready, it is selected and returned. It may happen that multiple
    /// operations are ready at the same time, in which case a random one among them is selected.
    ///
    /// The selected operation must be completed with [`SelectedOperation::send`]
    /// or [`SelectedOperation::recv`].
    ///
    /// # Panics
    ///
    /// Panics if no operations have been added to `Select`.
    pub fn select(&mut self) -> SelectedOperation<'a> {
        select(&mut self.handles, self.biased)
    }

    /// Blocks for a limited time until one of the operations becomes ready and selects it.
    ///
    /// The time is measured on `shuttle-time`'s clock: the selection times out only when no task
    /// can run (see the [crate documentation](crate#time)).
    pub fn select_timeout(&mut self, timeout: Duration) -> Result<SelectedOperation<'a>, SelectTimeoutError> {
        select_timeout(&mut self.handles, timeout, self.biased)
    }

    /// Blocks until a given deadline, or until one of the operations becomes ready and selects it.
    ///
    /// The deadline is an instant of `shuttle-time`'s clock: the selection times out only when no
    /// task can run (see the [crate documentation](crate#time)).
    pub fn select_deadline(&mut self, deadline: Instant) -> Result<SelectedOperation<'a>, SelectTimeoutError> {
        select_deadline(&mut self.handles, deadline, self.biased)
    }

    /// Attempts to find a ready operation without blocking.
    ///
    /// If an operation is ready, its index is returned. If multiple operations are ready at the
    /// same time, a random one among them is chosen. If none of the operations are ready, an error
    /// is returned.
    pub fn try_ready(&mut self) -> Result<usize, TryReadyError> {
        match run_ready(&mut self.handles, Timeout::Now, self.biased) {
            None => Err(TryReadyError),
            Some(index) => Ok(index),
        }
    }

    /// Blocks until one of the operations becomes ready.
    ///
    /// Once an operation becomes ready, its index is returned. If multiple operations are ready at
    /// the same time, a random one among them is chosen.
    ///
    /// # Panics
    ///
    /// Panics if no operations have been added to `Select`.
    pub fn ready(&mut self) -> usize {
        if self.handles.is_empty() {
            panic!("no operations have been added to `Select`");
        }
        run_ready(&mut self.handles, Timeout::Never, self.biased).expect("a wait without a timeout ends ready")
    }

    /// Blocks for a limited time until one of the operations becomes ready.
    ///
    /// The time is measured on `shuttle-time`'s clock: the wait times out only when no task can run
    /// (see the [crate documentation](crate#time)).
    pub fn ready_timeout(&mut self, timeout: Duration) -> Result<usize, ReadyTimeoutError> {
        match peek().checked_add(timeout) {
            Some(deadline) => self.ready_deadline(deadline),
            None => Ok(self.ready()),
        }
    }

    /// Blocks until a given deadline, or until one of the operations becomes ready.
    ///
    /// The deadline is an instant of `shuttle-time`'s clock: the wait times out only when no task
    /// can run (see the [crate documentation](crate#time)).
    pub fn ready_deadline(&mut self, deadline: Instant) -> Result<usize, ReadyTimeoutError> {
        match run_ready(&mut self.handles, Timeout::At(deadline), self.biased) {
            None => Err(ReadyTimeoutError),
            Some(index) => Ok(index),
        }
    }
}

impl<'a> Clone for Select<'a> {
    fn clone(&self) -> Select<'a> {
        Select {
            handles: self.handles.clone(),
            next_index: self.next_index,
            biased: self.biased,
        }
    }
}

impl<'a> Default for Select<'a> {
    fn default() -> Select<'a> {
        Select::new()
    }
}

impl fmt::Debug for Select<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Select { .. }")
    }
}

/// A selected operation that needs to be completed.
///
/// To complete the operation, call [`send`] or [`recv`].
///
/// # Panics
///
/// Forgetting to complete the operation is an error and might lead to deadlocks. If a
/// `SelectedOperation` is dropped without completion, a panic occurs.
///
/// [`send`]: SelectedOperation::send
/// [`recv`]: SelectedOperation::recv
#[must_use]
pub struct SelectedOperation<'a> {
    /// What completing the operation takes.
    token: Token,
    /// The index of the selected operation.
    index: usize,
    /// The address of the selected `Sender` or `Receiver`.
    ptr: *const u8,
    /// Indicates that `Sender`s and `Receiver`s are borrowed.
    _marker: PhantomData<&'a ()>,
}

impl SelectedOperation<'_> {
    fn new(token: Token, index: usize, ptr: *const u8) -> Self {
        Self {
            token,
            index,
            ptr,
            _marker: PhantomData,
        }
    }

    /// Returns the index of the selected operation.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Completes the send operation.
    ///
    /// The passed [`Sender`] reference must be the same one that was used in [`Select::send`]
    /// when the operation was added.
    ///
    /// # Panics
    ///
    /// Panics if an incorrect [`Sender`] reference is passed.
    pub fn send<T>(mut self, s: &Sender<T>, msg: T) -> Result<(), SendError<T>> {
        assert!(
            s as *const Sender<T> as *const u8 == self.ptr,
            "passed a sender that wasn't selected",
        );
        let token = std::mem::take(&mut self.token);
        std::mem::forget(self);
        s.write(token, msg).map_err(SendError)
    }

    /// Completes the receive operation.
    ///
    /// The passed [`Receiver`] reference must be the same one that was used in [`Select::recv`]
    /// when the operation was added.
    ///
    /// # Panics
    ///
    /// Panics if an incorrect [`Receiver`] reference is passed.
    pub fn recv<T>(mut self, r: &Receiver<T>) -> Result<T, RecvError> {
        assert!(
            r as *const Receiver<T> as *const u8 == self.ptr,
            "passed a receiver that wasn't selected",
        );
        let token = std::mem::take(&mut self.token);
        std::mem::forget(self);
        r.read(token).map_err(|()| RecvError)
    }
}

impl fmt::Debug for SelectedOperation<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("SelectedOperation { .. }")
    }
}

impl Drop for SelectedOperation<'_> {
    fn drop(&mut self) {
        // Not while a panic unwinds, such as that of `send` or `recv` given the wrong handle: a second
        // panic would abort the test process, as it does crossbeam's, instead of failing the test.
        if !std::thread::panicking() {
            panic!("dropped `SelectedOperation` without completing the operation");
        }
    }
}
