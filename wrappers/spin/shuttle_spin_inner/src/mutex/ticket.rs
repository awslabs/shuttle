//! A ticket-based mutex.
//!
//! Waiting threads take a 'ticket' from the lock in the order they arrive and gain access to the
//! lock when their ticket is next in the queue. Best-case latency is slightly worse than a regular
//! spinning mutex, but worse-case latency is infinitely better. Waiting threads simply need to
//! wait for all threads that come before them in the queue to finish.
//!
//! Under Shuttle the lock keeps spin's two counters, the next ticket and the ticket being served,
//! and a task whose ticket is not being served blocks until the holder moves the queue on.

use super::spin::{Inner, InnerGuard, fmt_mutex, fmt_value, leak_access};
use crate::state::{Core, State, lock_data, release_after_switch};
use crate::value::{Access, Copy, CopySlot, copied};
use crate::{RelaxStrategy, Spin};
use shuttle_engine::runtime::per_execution::ValueLock;
use shuttle_engine::runtime::thread;
use std::cell::{Cell, RefCell};
use std::fmt;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool as StdAtomicBool, Ordering as StdOrdering};

/// A spin-based [ticket lock](https://en.wikipedia.org/wiki/Ticket_lock) providing mutually
/// exclusive access to data.
///
/// A ticket lock is analogous to a queue management system for lock requests. When a thread tries
/// to take a lock, it is assigned a 'ticket'. It then spins until its ticket becomes next in line.
/// When the lock guard is released, the next ticket will be processed.
///
/// # Under Shuttle
///
/// [`lock`](Self::lock) takes one scheduling point, before it takes its ticket. A task whose ticket
/// is not being served blocks until the holder releases the lock; the tasks then get the lock in
/// the order of their tickets, as with spin. [`try_lock`](Self::try_lock),
/// [`is_locked`](Self::is_locked) and the release of a guard take one scheduling point each.
///
/// The state lives in the running execution, as `SpinMutex`'s does (see
/// [`SpinMutex`](crate::mutex::SpinMutex)).
pub struct TicketMutex<T: ?Sized, R = Spin> {
    phantom: PhantomData<R>,
    core: Core,
    /// Whether a guard was leaked. The lock then stays locked in every later execution, until
    /// [`force_unlock`](Self::force_unlock).
    leaked: StdAtomicBool,
    /// Keeps the lock to one execution at a time (see `SpinMutex`).
    value: ValueLock,
    data: Inner<T>,
}

/// The name of the lock in the messages of [`ValueLock::acquire`].
const KIND: &str = "spin TicketMutex";

/// A guard that protects some data.
///
/// When the guard is dropped, the next ticket will be processed.
pub struct TicketMutexGuard<'a, T: ?Sized + 'a> {
    access: Access<'a, T, InnerGuard<'a, T>>,
    core: &'a Core,
    leaked: &'a StdAtomicBool,
    // spin's guard holds a `&'a mut T`, which decides its auto traits.
    _marker: PhantomData<&'a mut T>,
}

unsafe impl<T: ?Sized + Send, R> Sync for TicketMutex<T, R> {}
unsafe impl<T: ?Sized + Send, R> Send for TicketMutex<T, R> {}

// What spin's guard has from its `&'a mut T`.
unsafe impl<T: ?Sized + Sync> Sync for TicketMutexGuard<'_, T> {}
unsafe impl<T: ?Sized + Send> Send for TicketMutexGuard<'_, T> {}

/// The turn: the ticket being served, the word being the next ticket.
struct Turn {
    serving: Cell<usize>,
    /// Tickets whose tasks execution teardown unwound while they waited. Their turn is skipped.
    forfeited: RefCell<Vec<usize>>,
    /// Keeps the execution's copy of the value, if it is copied.
    copy: CopySlot,
}

/// Moves the queue on to the next ticket that a task still waits with.
fn serve_next(state: &State<Turn>) {
    let mut forfeited = state.extra.forfeited.borrow_mut();
    let mut next = state.extra.serving.get().wrapping_add(1);
    while let Some(i) = forfeited.iter().position(|ticket| *ticket == next) {
        forfeited.swap_remove(i);
        next = next.wrapping_add(1);
    }
    state.extra.serving.set(next);
}

impl<T, R> TicketMutex<T, R> {
    /// Creates a new [`TicketMutex`] wrapping the supplied data.
    #[inline(always)]
    pub const fn new(data: T) -> Self {
        Self {
            phantom: PhantomData,
            core: Core::new(),
            leaked: StdAtomicBool::new(false),
            value: ValueLock::new(),
            data: Inner::new(data),
        }
    }

    /// Consumes this [`TicketMutex`] and unwraps the underlying data.
    #[inline(always)]
    pub fn into_inner(self) -> T {
        if let Some(copy) = self.copy() {
            // Safety: the copy is a valid value, which needs no dropping, so reading it out leaves
            // nothing behind to drop twice.
            return unsafe { copy.ptr::<T>().as_ptr().read() };
        }
        self.data.into_inner()
    }

    /// Returns a mutable pointer to the underying data.
    ///
    /// This is mostly meant to be used for applications which require manual unlocking, but where
    /// storing both the lock and the pointer to the inner data gets inefficient.
    #[inline(always)]
    pub fn as_mut_ptr(&self) -> *mut T {
        // Safety: nothing writes the value while an execution can reach it (see `CopySlot`), so the
        // copy can be made from it without spin's lock.
        if copied::<T>()
            && let Some(state) = self.try_enter()
            && let Some(copy) = state.extra.copy.get_or_copy(unsafe { &*self.data.as_mut_ptr() })
        {
            return copy.ptr::<T>().as_ptr();
        }
        self.data.as_mut_ptr()
    }
}

impl<T: ?Sized + fmt::Debug, R> fmt::Debug for TicketMutex<T, R> {
    /// Formats the value unless the lock is held, as spin does, without taking a scheduling point.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let state = self.try_enter();
        let copy = state.and_then(|state| state.extra.copy.get());
        let serving = state.map(|state| state.extra.serving.get());
        fmt_value(
            &self.data,
            state,
            copy,
            |next_ticket| Some(next_ticket) != serving,
            |value| fmt_mutex(value, f),
        )
    }
}

impl<T: ?Sized, R: RelaxStrategy> TicketMutex<T, R> {
    /// Locks the [`TicketMutex`] and returns a guard that permits access to the inner data.
    ///
    /// The returned data may be dereferenced for data access and the lock will be dropped when the
    /// guard falls out of scope.
    #[inline(always)]
    pub fn lock(&self) -> TicketMutexGuard<'_, T> {
        thread::switch();
        let state = self.enter();
        state.observe();
        let ticket = state.word.get();
        state.word.set(ticket.wrapping_add(1));
        state.publish();

        // If execution teardown unwinds the task while it waits, it gives up its turn, so that the
        // destructors that run later can still take the lock.
        struct Forfeit<'a>(&'a State<Turn>, usize);
        impl Drop for Forfeit<'_> {
            fn drop(&mut self) {
                if self.0.extra.serving.get() == self.1 {
                    self.0.release(serve_next);
                } else {
                    self.0.extra.forfeited.borrow_mut().push(self.1);
                }
            }
        }
        let forfeit = Forfeit(state, ticket);
        while state.extra.serving.get() != ticket {
            state.wait();
        }
        std::mem::forget(forfeit);

        state.observe();
        self.guard(state)
    }
}

impl<T: ?Sized, R> TicketMutex<T, R> {
    /// The state the lock starts an execution in: locked if a guard was leaked, unlocked
    /// otherwise.
    fn initial_state(&self) -> State<Turn> {
        let turn = Turn {
            serving: Cell::new(0),
            forfeited: RefCell::new(Vec::new()),
            copy: CopySlot::default(),
        };
        State::new(usize::from(self.leaked.load(StdOrdering::Acquire)), turn)
    }

    /// This execution's state of the lock.
    fn enter(&self) -> &State<Turn> {
        self.core.state(|| self.initial_state()).0
    }

    /// Like [`Self::enter`], but `None` outside of an execution.
    fn try_enter(&self) -> Option<&State<Turn>> {
        Some(self.core.try_state(|| self.initial_state())?.0)
    }

    /// This execution's copy of the value, if the value is copied and there is an execution.
    fn copy(&self) -> Option<&Copy> {
        if !copied::<T>() {
            return None;
        }
        self.try_enter()?.extra.copy.get()
    }

    fn guard(&self, state: &State<Turn>) -> TicketMutexGuard<'_, T> {
        let guard = lock_data(&self.value, KIND, || self.data.try_lock());
        let copy = state.extra.copy.get_or_copy(&*guard);
        TicketMutexGuard {
            access: Access::new(guard, copy, &self.value),
            core: &self.core,
            leaked: &self.leaked,
            _marker: PhantomData,
        }
    }

    /// Returns `true` if the lock is currently held.
    ///
    /// # Safety
    ///
    /// This function provides no synchronization guarantees and so its result should be considered
    /// 'out of date' the instant it is called. Do not use it for synchronization purposes. However,
    /// it may be useful as a heuristic.
    ///
    /// Under Shuttle this takes a scheduling point, as the load of an atomic does.
    #[inline(always)]
    pub fn is_locked(&self) -> bool {
        thread::switch();
        let state = self.enter();
        state.observe();
        state.word.get() != state.extra.serving.get()
    }

    /// Force unlock this [`TicketMutex`], by serving the next ticket.
    ///
    /// # Safety
    ///
    /// This is *extremely* unsafe if the lock is not held by the current thread. However, this can
    /// be useful in some instances for exposing the lock to FFI that doesn't know how to deal with
    /// RAII.
    #[inline(always)]
    pub unsafe fn force_unlock(&self) {
        release_after_switch(|| {
            // Enter first: if this is the first use of the lock in the execution, the state starts
            // out locked by the leaked guard that this releases.
            let state = self.try_enter();
            self.leaked.store(false, StdOrdering::Release);
            // Safety: the caller's.
            unsafe { self.data.force_unlock() };
            // The leaked or forgotten guard's hold on the value.
            self.value.release();
            if let Some(state) = state {
                state.release(serve_next);
            }
        });
    }

    /// Try to lock this [`TicketMutex`], returning a lock guard if successful.
    #[inline(always)]
    pub fn try_lock(&self) -> Option<TicketMutexGuard<'_, T>> {
        thread::switch();
        let state = self.enter();
        state.observe();
        let ticket = state.word.get();
        if ticket != state.extra.serving.get() {
            return None;
        }
        state.word.set(ticket.wrapping_add(1));
        state.publish();
        Some(self.guard(state))
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the [`TicketMutex`] mutably, and a mutable reference is guaranteed
    /// to be exclusive in Rust, no actual locking needs to take place -- the mutable borrow
    /// statically guarantees no locks exist. As such, this is a 'zero-cost' operation.
    #[inline(always)]
    pub fn get_mut(&mut self) -> &mut T {
        if copied::<T>() {
            let value: *const T = self.data.get_mut();
            if let Some(state) = self.try_enter() {
                // Safety: `value` is the exclusive borrow's, only read to make the copy.
                if let Some(copy) = state.extra.copy.get_or_copy(unsafe { &*value }) {
                    // Safety: the `&mut` borrow rules out any guard, and so any other reference to
                    // the copy.
                    return unsafe { &mut *copy.ptr::<T>().as_ptr() };
                }
            }
        }
        self.data.get_mut()
    }
}

// spin writes `T: ?Sized + Default`, which means the same, since `Default` implies `Sized`.
impl<T: Default, R> Default for TicketMutex<T, R> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<T, R> From<T> for TicketMutex<T, R> {
    fn from(data: T) -> Self {
        Self::new(data)
    }
}

impl<'a, T: ?Sized> TicketMutexGuard<'a, T> {
    /// Leak the lock guard, yielding a mutable reference to the underlying data.
    ///
    /// Note that this function will permanently lock the original [`TicketMutex`], also in later
    /// Shuttle executions.
    #[inline(always)]
    pub fn leak(this: Self) -> &'a mut T {
        let mut this = ManuallyDrop::new(this);
        this.leaked.store(true, StdOrdering::Release);
        let copy = this
            .core
            .existing_state::<Turn>()
            .and_then(|state| state.extra.copy.get());
        // Safety: `this` is never used or dropped again.
        unsafe { leak_access(&mut this.access, copy) }
    }
}

impl<'a, T: ?Sized + fmt::Debug> fmt::Debug for TicketMutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'a, T: ?Sized + fmt::Display> fmt::Display for TicketMutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<'a, T: ?Sized> Deref for TicketMutexGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.access.get()
    }
}

impl<'a, T: ?Sized> DerefMut for TicketMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.access.get_mut()
    }
}

impl<'a, T: ?Sized> Drop for TicketMutexGuard<'a, T> {
    /// Serves the next ticket. A scheduling point.
    fn drop(&mut self) {
        let core = self.core;
        let access = &mut self.access;
        release_after_switch(|| {
            // Safety: the guard is being dropped, so nothing uses `access` again.
            unsafe { access.release() };
            if let Some(state) = core.existing_state::<Turn>() {
                state.release(serve_next);
            }
        });
    }
}

#[cfg(feature = "lock_api")]
unsafe impl<R: RelaxStrategy> lock_api_crate::RawMutex for TicketMutex<(), R> {
    type GuardMarker = lock_api_crate::GuardSend;

    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = Self::new(());

    fn lock(&self) {
        // Prevent guard destructor running
        core::mem::forget(Self::lock(self));
    }

    fn try_lock(&self) -> bool {
        // Prevent guard destructor running
        Self::try_lock(self).map(core::mem::forget).is_some()
    }

    unsafe fn unlock(&self) {
        // Safety: the caller's.
        unsafe { self.force_unlock() };
    }

    fn is_locked(&self) -> bool {
        Self::is_locked(self)
    }
}
