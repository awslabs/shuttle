//! A spinning mutex with a fairer unlock algorithm.
//!
//! This mutex is similar to the `SpinMutex` in that it uses spinning to avoid context switches.
//! However, it uses a fairer unlock algorithm that avoids starvation of threads that are waiting
//! for the lock.
//!
//! Under Shuttle the lock word is spin's: a locked bit, and the number of starving users above it.

use super::spin::{Inner, InnerGuard, fmt_value, leak_access};
use crate::state::{Core, State, lock_data, release_after_switch};
use crate::value::{Access, Copy, CopySlot, copied};
use crate::{RelaxStrategy, Spin};
use shuttle_engine::runtime::per_execution::ValueLock;
use shuttle_engine::runtime::thread;
use std::fmt;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool as StdAtomicBool, Ordering as StdOrdering};

// The lowest bit of the word says whether the mutex is locked. The rest of the bits are the number
// of starving users.
const LOCKED: usize = 1;
const STARVED: usize = 2;

/// A [spin lock](https://en.m.wikipedia.org/wiki/Spinlock) providing mutually exclusive access to
/// data, but with a fairer algorithm.
///
/// # Under Shuttle
///
/// [`lock`](Self::lock) takes one scheduling point, and a task that cannot take the lock blocks
/// until the word changes. spin's `lock` turns its caller into a starving user after spinning 1,024
/// times, from when on it goes before new arrivals. Shuttle has no measure of how long a task has
/// waited, so under Shuttle `lock` never starves on its own: a waiting task can be overtaken any
/// number of times. A task that calls [`starve`](Self::starve) itself gets spin's treatment.
/// [`try_lock`](Self::try_lock), [`try_lock_starver`](Self::try_lock_starver),
/// [`starve`](Self::starve), [`is_locked`](Self::is_locked), the methods of [`Starvation`], and
/// the release of a guard or a [`Starvation`] take one scheduling point each.
///
/// The state lives in the running execution, as `SpinMutex`'s does (see
/// [`SpinMutex`](crate::mutex::SpinMutex)).
pub struct FairMutex<T: ?Sized, R = Spin> {
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
const KIND: &str = "spin FairMutex";

/// A guard that provides mutable data access.
///
/// When the guard falls out of scope it will release the lock.
pub struct FairMutexGuard<'a, T: ?Sized + 'a> {
    access: Access<'a, T, InnerGuard<'a, T>>,
    core: &'a Core,
    leaked: &'a StdAtomicBool,
}

/// A handle that indicates that we have been trying to acquire the lock for a while.
///
/// This handle is used to prevent starvation.
pub struct Starvation<'a, T: ?Sized + 'a, R> {
    lock: &'a FairMutex<T, R>,
}

/// Indicates whether a lock was rejected due to the lock being held by another thread or due to
/// starvation.
#[derive(Debug)]
pub enum LockRejectReason {
    /// The lock was rejected due to the lock being held by another thread.
    Locked,

    /// The lock was rejected due to starvation.
    Starved,
}

// Same unsafe impls as `std::sync::Mutex`
unsafe impl<T: ?Sized + Send, R> Sync for FairMutex<T, R> {}
unsafe impl<T: ?Sized + Send, R> Send for FairMutex<T, R> {}

unsafe impl<T: ?Sized + Sync> Sync for FairMutexGuard<'_, T> {}
unsafe impl<T: ?Sized + Send> Send for FairMutexGuard<'_, T> {}

/// A fair mutex's state in one execution: spin's word, and where the execution keeps its copy of
/// the value, if it is copied.
type FairState = State<CopySlot>;

impl<T, R> FairMutex<T, R> {
    /// Creates a new [`FairMutex`] wrapping the supplied data.
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

    /// Consumes this [`FairMutex`] and unwraps the underlying data.
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

impl<T: ?Sized, R: RelaxStrategy> FairMutex<T, R> {
    /// Locks the [`FairMutex`] and returns a guard that permits access to the inner data.
    ///
    /// The returned value may be dereferenced for data access and the lock will be dropped when
    /// the guard falls out of scope.
    #[inline(always)]
    pub fn lock(&self) -> FairMutexGuard<'_, T> {
        thread::switch();
        let state = self.enter();
        loop {
            state.observe();
            if state.word.get() == 0 {
                state.word.set(LOCKED);
                state.publish();
                return self.guard(state);
            }
            state.wait();
        }
    }
}

impl<T: ?Sized, R> FairMutex<T, R> {
    /// The state the lock starts an execution in: locked if a guard was leaked, unlocked
    /// otherwise, with no starving users.
    fn initial_state(&self) -> FairState {
        State::new(usize::from(self.leaked.load(StdOrdering::Acquire)), CopySlot::default())
    }

    /// This execution's state of the lock.
    fn enter(&self) -> &FairState {
        self.core.state(|| self.initial_state()).0
    }

    /// Like [`Self::enter`], but `None` outside of an execution.
    fn try_enter(&self) -> Option<&FairState> {
        Some(self.core.try_state(|| self.initial_state())?.0)
    }

    /// This execution's copy of the value, if the value is copied and there is an execution.
    fn copy(&self) -> Option<&Copy> {
        if !copied::<T>() {
            return None;
        }
        self.try_enter()?.extra.get()
    }

    fn guard(&self, state: &FairState) -> FairMutexGuard<'_, T> {
        let guard = lock_data(&self.value, KIND, || self.data.try_lock());
        let copy = state.extra.get_or_copy(&*guard);
        FairMutexGuard {
            access: Access::new(guard, copy, &self.value),
            core: &self.core,
            leaked: &self.leaked,
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
        state.word.get() & LOCKED != 0
    }

    /// Force unlock this [`FairMutex`].
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
                state.release(|state| state.word.set(state.word.get() & !LOCKED));
            }
        });
    }

    /// Try to lock this [`FairMutex`], returning a lock guard if successful.
    #[inline(always)]
    pub fn try_lock(&self) -> Option<FairMutexGuard<'_, T>> {
        self.try_lock_starver().ok()
    }

    /// Tries to lock this [`FairMutex`] and returns a result that indicates whether the lock was
    /// rejected due to a starver or not.
    #[inline(always)]
    pub fn try_lock_starver(&self) -> Result<FairMutexGuard<'_, T>, LockRejectReason> {
        thread::switch();
        let state = self.enter();
        state.observe();
        match state.word.get() {
            0 => {
                state.word.set(LOCKED);
                state.publish();
                Ok(self.guard(state))
            }
            LOCKED => Err(LockRejectReason::Locked),
            _ => Err(LockRejectReason::Starved),
        }
    }

    /// Indicates that the current user has been waiting for the lock for a while and that the lock
    /// should yield to this thread over a newly arriving thread.
    pub fn starve(&self) -> Starvation<'_, T, R> {
        thread::switch();
        let state = self.enter();
        state.observe();
        // Add a new starver to the state.
        let word = state.word.get();
        if word > (isize::MAX - 1) as usize {
            // spin aborts the process here.
            panic!("aborting due to invalid operation");
        }
        state.word.set(word + STARVED);
        state.publish();
        Starvation { lock: self }
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the [`FairMutex`] mutably, and a mutable reference is guaranteed to
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

impl<T: ?Sized + fmt::Debug, R> fmt::Debug for FairMutex<T, R> {
    /// Formats the value if the lock could be taken (no holder and no starving user), as spin's
    /// does, without taking a scheduling point.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        struct LockWrapper<'a, T: ?Sized + fmt::Debug>(Option<&'a T>);

        impl<T: ?Sized + fmt::Debug> fmt::Debug for LockWrapper<'_, T> {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                match &self.0 {
                    Some(value) => fmt::Debug::fmt(value, f),
                    None => f.write_str("<locked>"),
                }
            }
        }

        let state = self.try_enter();
        let copy = state.and_then(|state| state.extra.get());
        fmt_value(
            &self.data,
            state,
            copy,
            |word| word != 0,
            |value| f.debug_struct("FairMutex").field("data", &LockWrapper(value)).finish(),
        )
    }
}

// spin writes `T: ?Sized + Default`, which means the same, since `Default` implies `Sized`.
impl<T: Default, R> Default for FairMutex<T, R> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<T, R> From<T> for FairMutex<T, R> {
    fn from(data: T) -> Self {
        Self::new(data)
    }
}

impl<'a, T: ?Sized> FairMutexGuard<'a, T> {
    /// Leak the lock guard, yielding a mutable reference to the underlying data.
    ///
    /// Note that this function will permanently lock the original [`FairMutex`], also in later
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

impl<'a, T: ?Sized + fmt::Debug> fmt::Debug for FairMutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'a, T: ?Sized + fmt::Display> fmt::Display for FairMutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<'a, T: ?Sized> Deref for FairMutexGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.access.get()
    }
}

impl<'a, T: ?Sized> DerefMut for FairMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.access.get_mut()
    }
}

impl<'a, T: ?Sized> Drop for FairMutexGuard<'a, T> {
    /// The dropping of the MutexGuard will release the lock it was created from. A scheduling
    /// point.
    fn drop(&mut self) {
        let core = self.core;
        let access = &mut self.access;
        release_after_switch(|| {
            // Safety: the guard is being dropped, so nothing uses `access` again.
            unsafe { access.release() };
            if let Some(state) = core.existing_state::<CopySlot>() {
                state.release(|state| state.word.set(state.word.get() & !LOCKED));
            }
        });
    }
}

impl<'a, T: ?Sized, R> Starvation<'a, T, R> {
    /// Takes the lock for the starving user, who stops starving.
    fn acquire(self, state: &FairState) -> FairMutexGuard<'a, T> {
        // One step for what spin does in two, taking the lock and then dropping the handle, which
        // decrements the number of starving users: in between, the word only refuses more.
        state.word.set((state.word.get() | LOCKED) - STARVED);
        state.publish();
        let lock = self.lock;
        std::mem::forget(self);
        lock.guard(state)
    }

    /// Attempts the lock the mutex if we are the only starving user.
    ///
    /// This allows another user to lock the mutex if they are starving as well.
    pub fn try_lock_fair(self) -> Result<FairMutexGuard<'a, T>, Self> {
        thread::switch();
        let state = self.lock.enter();
        state.observe();
        if state.word.get() == STARVED {
            // We are the only starving user, lock the mutex.
            Ok(self.acquire(state))
        } else {
            // Another user is starving, fail.
            Err(self)
        }
    }

    /// Attempts to lock the mutex.
    ///
    /// If the lock is currently held by another thread, this will return `None`.
    pub fn try_lock(self) -> Result<FairMutexGuard<'a, T>, Self> {
        thread::switch();
        let state = self.lock.enter();
        state.observe();
        if state.word.get() & LOCKED == 0 {
            Ok(self.acquire(state))
        } else {
            Err(self)
        }
    }
}

impl<'a, T: ?Sized, R: RelaxStrategy> Starvation<'a, T, R> {
    /// Locks the mutex.
    pub fn lock(self) -> FairMutexGuard<'a, T> {
        thread::switch();
        let state = self.lock.enter();
        loop {
            state.observe();
            if state.word.get() & LOCKED == 0 {
                return self.acquire(state);
            }
            state.wait();
        }
    }
}

impl<'a, T: ?Sized, R> Drop for Starvation<'a, T, R> {
    /// As there is no longer a user being starved, decrements the starver count. A scheduling
    /// point.
    fn drop(&mut self) {
        let core = &self.lock.core;
        release_after_switch(|| {
            if let Some(state) = core.existing_state::<CopySlot>() {
                state.release(|state| state.word.set(state.word.get() - STARVED));
            }
        });
    }
}

impl fmt::Display for LockRejectReason {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LockRejectReason::Locked => write!(f, "locked"),
            LockRejectReason::Starved => write!(f, "starved"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for LockRejectReason {}

#[cfg(feature = "lock_api")]
unsafe impl<R: RelaxStrategy> lock_api_crate::RawMutex for FairMutex<(), R> {
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
