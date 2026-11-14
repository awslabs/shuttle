//! A naïve spinning mutex.
//!
//! Waiting threads hammer an atomic variable until it becomes available. Best-case latency is low,
//! but worst-case latency is theoretically infinite.
//!
//! Under Shuttle the lock word is `false` or `true`, as spin's is, and a task that finds the lock
//! held blocks until it is released instead of spinning.

use crate::state::{Core, State, lock_data, release_after_switch};
use crate::value::{Access, Copy, CopySlot, copied};
use crate::{RelaxStrategy, Spin};
use shuttle_engine::runtime::per_execution::ValueLock;
use shuttle_engine::runtime::thread;
use spin_orig::mutex::{SpinMutex as Data, SpinMutexGuard as DataGuard};
use std::fmt;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool as StdAtomicBool, Ordering as StdOrdering};

/// spin's lock around the value (see the `value` module). Its relax strategy is never used: a task
/// takes it only once it holds the lock in the execution and its `ValueLock`, so it is free then,
/// unless a `Debug` on another thread holds it for a moment.
pub(crate) type Inner<T> = Data<T, spin_orig::relax::Loop>;
pub(crate) type InnerGuard<'a, T> = DataGuard<'a, T>;

/// A [spin lock](https://en.m.wikipedia.org/wiki/Spinlock) providing mutually exclusive access to
/// data.
///
/// # Under Shuttle
///
/// [`lock`](Self::lock), [`try_lock`](Self::try_lock), [`try_lock_weak`](Self::try_lock_weak),
/// [`is_locked`](Self::is_locked) and the release of a guard each take one scheduling point. A task
/// that finds the lock held blocks until the holder releases it, and any of the tasks that wait can
/// take it next, as with spin. `try_lock_weak` never fails spuriously.
///
/// The lock's state lives in the running execution, so a `static` lock is unlocked at the start of
/// every execution, and its value starts afresh if it needs no dropping (see the crate
/// documentation). A guard that was [leaked](SpinMutexGuard::leak) keeps the lock locked in later
/// executions, as it would with spin. Two executions can't hold the lock at once: one that takes
/// it while a guard of another execution holds it panics, saying whose guard it is, one of a test
/// that runs at the same time on another thread or a forgotten one that an ended execution left
/// behind (see the crate documentation).
pub struct SpinMutex<T: ?Sized, R = Spin> {
    phantom: PhantomData<R>,
    core: Core,
    /// Whether a guard was leaked. The lock then stays locked in every later execution, until
    /// [`force_unlock`](Self::force_unlock).
    leaked: StdAtomicBool,
    /// Keeps the lock to one execution at a time, which the state, being per execution, can't.
    value: ValueLock,
    data: Inner<T>,
}

/// The name of the lock in the messages of [`ValueLock::acquire`].
const KIND: &str = "spin SpinMutex";

/// A guard that provides mutable data access.
///
/// When the guard falls out of scope it will release the lock.
pub struct SpinMutexGuard<'a, T: ?Sized + 'a> {
    access: Access<'a, T, InnerGuard<'a, T>>,
    core: &'a Core,
    leaked: &'a StdAtomicBool,
}

// Same unsafe impls as `std::sync::Mutex`
unsafe impl<T: ?Sized + Send, R> Sync for SpinMutex<T, R> {}
unsafe impl<T: ?Sized + Send, R> Send for SpinMutex<T, R> {}

unsafe impl<T: ?Sized + Sync> Sync for SpinMutexGuard<'_, T> {}
unsafe impl<T: ?Sized + Send> Send for SpinMutexGuard<'_, T> {}

/// A mutex's state in one execution: the word is `0` while the mutex is unlocked and `1` while it
/// is locked; the extra keeps the execution's copy of the value, if it is copied.
type MutexState = State<CopySlot>;

impl<T, R> SpinMutex<T, R> {
    /// Creates a new [`SpinMutex`] wrapping the supplied data.
    #[inline(always)]
    pub const fn new(data: T) -> Self {
        SpinMutex {
            phantom: PhantomData,
            core: Core::new(),
            leaked: StdAtomicBool::new(false),
            value: ValueLock::new(),
            data: Data::new(data),
        }
    }

    /// Consumes this [`SpinMutex`] and unwraps the underlying data.
    #[inline(always)]
    pub fn into_inner(self) -> T {
        if let Some(copy) = self.copy() {
            // Safety: the copy is a valid value, which needs no dropping, so reading it out leaves
            // nothing behind to drop twice.
            return unsafe { copy.ptr::<T>().as_ptr().read() };
        }
        self.data.into_inner()
    }

    /// Returns a mutable pointer to the underlying data.
    ///
    /// This is mostly meant to be used for applications which require manual unlocking, but where
    /// storing both the lock and the pointer to the inner data gets inefficient.
    ///
    /// Under Shuttle, inside an execution, the pointer is to the execution's copy of a value that
    /// needs no dropping, and valid for that execution only.
    #[inline(always)]
    pub fn as_mut_ptr(&self) -> *mut T {
        // Safety: nothing writes the value while an execution can reach it (see `CopySlot`), so the
        // copy can be made from it without spin's lock.
        if copied::<T>()
            && let Some(state) = self.try_enter()
            && let Some(copy) = state.extra.get_or_copy(unsafe { &*self.data.as_mut_ptr() })
        {
            return copy.ptr::<T>().as_ptr();
        }
        self.data.as_mut_ptr()
    }
}

impl<T: ?Sized, R: RelaxStrategy> SpinMutex<T, R> {
    /// Locks the [`SpinMutex`] and returns a guard that permits access to the inner data.
    ///
    /// The returned value may be dereferenced for data access and the lock will be dropped when
    /// the guard falls out of scope.
    #[inline(always)]
    pub fn lock(&self) -> SpinMutexGuard<'_, T> {
        thread::switch();
        let state = self.enter();
        loop {
            if let Some(guard) = self.acquire(state) {
                return guard;
            }
            state.wait();
        }
    }
}

impl<T: ?Sized, R> SpinMutex<T, R> {
    fn initial_state(&self) -> MutexState {
        // A leaked guard holds the lock in every execution.
        State::new(usize::from(self.leaked.load(StdOrdering::Acquire)), CopySlot::default())
    }

    /// This execution's state of the lock.
    fn enter(&self) -> &MutexState {
        self.core.state(|| self.initial_state()).0
    }

    /// Like [`Self::enter`], but `None` outside of an execution.
    fn try_enter(&self) -> Option<&MutexState> {
        Some(self.core.try_state(|| self.initial_state())?.0)
    }

    /// This execution's copy of the value, if the value is copied, there is an execution, and it
    /// has made its copy. Until then, the value is what the copy would be (see `CopySlot`).
    fn copy(&self) -> Option<&Copy> {
        if !copied::<T>() {
            return None;
        }
        self.try_enter()?.extra.get()
    }

    /// Takes the lock if it is free. One step: the caller has taken the scheduling point.
    fn acquire(&self, state: &MutexState) -> Option<SpinMutexGuard<'_, T>> {
        state.observe();
        if state.word.get() != 0 {
            return None;
        }
        state.word.set(1);
        state.publish();
        let guard = lock_data(&self.value, KIND, || self.data.try_lock());
        let copy = state.extra.get_or_copy(&*guard);
        Some(SpinMutexGuard {
            access: Access::new(guard, copy, &self.value),
            core: &self.core,
            leaked: &self.leaked,
        })
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
        state.word.get() != 0
    }

    /// Force unlock this [`SpinMutex`].
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
                state.release(|state| state.word.set(0));
            }
        });
    }

    /// Try to lock this [`SpinMutex`], returning a lock guard if successful.
    #[inline(always)]
    pub fn try_lock(&self) -> Option<SpinMutexGuard<'_, T>> {
        thread::switch();
        let state = self.enter();
        self.acquire(state)
    }

    /// Try to lock this [`SpinMutex`], returning a lock guard if successful.
    ///
    /// Unlike [`SpinMutex::try_lock`], this function is allowed to spuriously fail even when the
    /// mutex is unlocked, which can result in more efficient code on some platforms. Under Shuttle
    /// it never fails spuriously.
    #[inline(always)]
    pub fn try_lock_weak(&self) -> Option<SpinMutexGuard<'_, T>> {
        self.try_lock()
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the [`SpinMutex`] mutably, and a mutable reference is guaranteed to
    /// be exclusive in Rust, no actual locking needs to take place -- the mutable borrow statically
    /// guarantees no locks exist. As such, this is a 'zero-cost' operation.
    #[inline(always)]
    pub fn get_mut(&mut self) -> &mut T {
        if copied::<T>() {
            let value: *const T = self.data.get_mut();
            if let Some(state) = self.try_enter() {
                // Safety: `value` is the exclusive borrow's, only read to make the copy.
                if let Some(copy) = state.extra.get_or_copy(unsafe { &*value }) {
                    // Safety: the `&mut` borrow rules out any guard, and so any other reference to
                    // the copy.
                    return unsafe { &mut *copy.ptr::<T>().as_ptr() };
                }
            }
        }
        self.data.get_mut()
    }
}

/// Formats a mutex as spin does: its value, or that it is locked.
pub(crate) fn fmt_mutex<T: ?Sized + fmt::Debug>(value: Option<&T>, f: &mut fmt::Formatter) -> fmt::Result {
    match value {
        Some(value) => write!(f, "Mutex {{ data: ")
            .and_then(|()| value.fmt(f))
            .and_then(|()| write!(f, " }}")),
        None => write!(f, "Mutex {{ <locked> }}"),
    }
}

/// Reaches a mutex's value for formatting, without taking a scheduling point: `None` while it is
/// held. `state` is the mutex's state in this execution, if there is an execution, and `held`
/// says from its word whether the mutex is held.
pub(crate) fn fmt_value<T: ?Sized, X>(
    data: &Inner<T>,
    state: Option<&State<X>>,
    copy: Option<&Copy>,
    held: impl FnOnce(usize) -> bool,
    fmt: impl FnOnce(Option<&T>) -> fmt::Result,
) -> fmt::Result {
    if let Some(state) = state {
        if held(state.word.get()) {
            return fmt(None);
        }
        if let Some(copy) = copy {
            // Safety: no task holds the lock, so nothing changes the copy while it is formatted.
            return fmt(Some(unsafe { copy.ptr::<T>().as_ref() }));
        }
    }
    // spin's lock around the value is held only by a task that holds the lock in this execution,
    // or from outside of it.
    let guard = data.try_lock();
    fmt(guard.as_deref())
}

impl<T: ?Sized + fmt::Debug, R> fmt::Debug for SpinMutex<T, R> {
    /// Formats the value unless the lock is held, as spin does, without taking a scheduling point.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let state = self.try_enter();
        let copy = state.and_then(|state| state.extra.get());
        fmt_value(&self.data, state, copy, |word| word != 0, |value| fmt_mutex(value, f))
    }
}

// spin writes `T: ?Sized + Default`, which means the same, since `Default` implies `Sized`.
impl<T: Default, R> Default for SpinMutex<T, R> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<T, R> From<T> for SpinMutex<T, R> {
    fn from(data: T) -> Self {
        Self::new(data)
    }
}

/// Leaks a guard's access: spin's lock around the value stays locked, and the execution's copy, if
/// the guard reaches one, outlives the execution.
///
/// # Safety
///
/// Nothing uses the guard that `access` belongs to again.
pub(crate) unsafe fn leak_access<'a, T: ?Sized>(
    access: &mut Access<'a, T, InnerGuard<'a, T>>,
    copy: Option<&Copy>,
) -> &'a mut T {
    // Safety: the caller's.
    match unsafe { access.leak() } {
        Ok(value) => {
            copy.expect("a guard of a copy belongs to an execution with that copy")
                .leak();
            // Safety: the copy outlives the execution now, and the lock stays locked, so nothing
            // else reaches it.
            unsafe { &mut *value.as_ptr() }
        }
        Err(guard) => InnerGuard::leak(guard),
    }
}

impl<'a, T: ?Sized> SpinMutexGuard<'a, T> {
    /// Leak the lock guard, yielding a mutable reference to the underlying data.
    ///
    /// Note that this function will permanently lock the original [`SpinMutex`], also in later
    /// Shuttle executions.
    #[inline(always)]
    pub fn leak(this: Self) -> &'a mut T {
        let mut this = ManuallyDrop::new(this);
        this.leaked.store(true, StdOrdering::Release);
        let copy = this
            .core
            .existing_state::<CopySlot>()
            .and_then(|state| state.extra.get());
        // Safety: `this` is never used or dropped again.
        unsafe { leak_access(&mut this.access, copy) }
    }
}

impl<'a, T: ?Sized + fmt::Debug> fmt::Debug for SpinMutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'a, T: ?Sized + fmt::Display> fmt::Display for SpinMutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<'a, T: ?Sized> Deref for SpinMutexGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.access.get()
    }
}

impl<'a, T: ?Sized> DerefMut for SpinMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.access.get_mut()
    }
}

impl<'a, T: ?Sized> Drop for SpinMutexGuard<'a, T> {
    /// The dropping of the MutexGuard will release the lock it was created from. A scheduling
    /// point.
    fn drop(&mut self) {
        let core = self.core;
        let access = &mut self.access;
        release_after_switch(|| {
            // Safety: the guard is being dropped, so nothing uses `access` again.
            unsafe { access.release() };
            if let Some(state) = core.existing_state::<CopySlot>() {
                state.release(|state| state.word.set(0));
            }
        });
    }
}

#[cfg(feature = "lock_api")]
unsafe impl<R: RelaxStrategy> lock_api_crate::RawMutex for SpinMutex<(), R> {
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
