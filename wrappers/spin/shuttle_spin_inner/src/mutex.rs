//! Locks that have the same behaviour as a mutex.
//!
//! The [`Mutex`] in the root of the crate, can be configured using the `ticket_mutex` feature.
//! If it's enabled, [`TicketMutex`] and [`TicketMutexGuard`] will be re-exported as [`Mutex`]
//! and [`MutexGuard`], otherwise the [`SpinMutex`] and guard will be re-exported.
//!
//! `ticket_mutex` is disabled by default.
//!
//! [`Mutex`]: ../struct.Mutex.html
//! [`MutexGuard`]: ../struct.MutexGuard.html
//! [`TicketMutex`]: ./struct.TicketMutex.html
//! [`TicketMutexGuard`]: ./struct.TicketMutexGuard.html
//! [`SpinMutex`]: ./struct.SpinMutex.html
//! [`SpinMutexGuard`]: ./struct.SpinMutexGuard.html

#[cfg(feature = "spin_mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "spin_mutex")))]
pub mod spin;
#[cfg(feature = "spin_mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "spin_mutex")))]
pub use self::spin::{SpinMutex, SpinMutexGuard};

#[cfg(feature = "ticket_mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "ticket_mutex")))]
pub mod ticket;
#[cfg(feature = "ticket_mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "ticket_mutex")))]
pub use self::ticket::{TicketMutex, TicketMutexGuard};

#[cfg(feature = "fair_mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "fair_mutex")))]
pub mod fair;
#[cfg(feature = "fair_mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "fair_mutex")))]
pub use self::fair::{FairMutex, FairMutexGuard, Starvation};

use crate::{RelaxStrategy, Spin};
use core::{
    fmt,
    ops::{Deref, DerefMut},
};

#[cfg(all(not(feature = "spin_mutex"), not(feature = "use_ticket_mutex")))]
compile_error!(
    "The `mutex` feature flag was used (perhaps through another feature?) without either `spin_mutex` or `use_ticket_mutex`. One of these is required."
);

#[cfg(all(not(feature = "use_ticket_mutex"), feature = "spin_mutex"))]
type InnerMutex<T, R> = self::spin::SpinMutex<T, R>;
#[cfg(all(not(feature = "use_ticket_mutex"), feature = "spin_mutex"))]
type InnerMutexGuard<'a, T> = self::spin::SpinMutexGuard<'a, T>;

#[cfg(feature = "use_ticket_mutex")]
type InnerMutex<T, R> = self::ticket::TicketMutex<T, R>;
#[cfg(feature = "use_ticket_mutex")]
type InnerMutexGuard<'a, T> = self::ticket::TicketMutexGuard<'a, T>;

/// A spin-based lock providing mutually exclusive access to data.
///
/// The implementation uses either a ticket mutex or a regular spin mutex depending on whether the
/// `spin_mutex` or `ticket_mutex` feature flag is enabled.
///
/// # Under Shuttle
///
/// Each operation takes the scheduling points of the mutex it is (see [`SpinMutex`] and
/// `TicketMutex`), and its state lives in the running execution, so a
/// `static` mutex is unlocked at the start of every execution.
pub struct Mutex<T: ?Sized, R = Spin> {
    inner: InnerMutex<T, R>,
}

unsafe impl<T: ?Sized + Send, R> Sync for Mutex<T, R> {}
unsafe impl<T: ?Sized + Send, R> Send for Mutex<T, R> {}

/// A generic guard that will protect some data access and uses either a ticket lock or a normal
/// spin mutex.
///
/// For more info see [`TicketMutexGuard`] or [`SpinMutexGuard`].
///
/// [`TicketMutexGuard`]: ./struct.TicketMutexGuard.html
/// [`SpinMutexGuard`]: ./struct.SpinMutexGuard.html
pub struct MutexGuard<'a, T: 'a + ?Sized> {
    inner: InnerMutexGuard<'a, T>,
}

impl<T, R> Mutex<T, R> {
    /// Creates a new [`Mutex`] wrapping the supplied data.
    #[inline(always)]
    pub const fn new(value: T) -> Self {
        Self {
            inner: InnerMutex::new(value),
        }
    }

    /// Consumes this [`Mutex`] and unwraps the underlying data.
    #[inline(always)]
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

impl<T: ?Sized, R: RelaxStrategy> Mutex<T, R> {
    /// Locks the [`Mutex`] and returns a guard that permits access to the inner data.
    ///
    /// The returned value may be dereferenced for data access and the lock will be dropped when
    /// the guard falls out of scope.
    #[inline(always)]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        MutexGuard {
            inner: self.inner.lock(),
        }
    }
}

impl<T: ?Sized, R> Mutex<T, R> {
    /// Returns `true` if the lock is currently held.
    ///
    /// # Safety
    ///
    /// This function provides no synchronization guarantees and so its result should be considered
    /// 'out of date' the instant it is called. Do not use it for synchronization purposes. However,
    /// it may be useful as a heuristic.
    #[inline(always)]
    pub fn is_locked(&self) -> bool {
        self.inner.is_locked()
    }

    /// Force unlock this [`Mutex`].
    ///
    /// # Safety
    ///
    /// This is *extremely* unsafe if the lock is not held by the current thread. However, this can
    /// be useful in some instances for exposing the lock to FFI that doesn't know how to deal with
    /// RAII.
    #[inline(always)]
    pub unsafe fn force_unlock(&self) {
        // Safety: the caller's.
        unsafe { self.inner.force_unlock() }
    }

    /// Try to lock this [`Mutex`], returning a lock guard if successful.
    #[inline(always)]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.inner.try_lock().map(|guard| MutexGuard { inner: guard })
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the [`Mutex`] mutably, and a mutable reference is guaranteed to be
    /// exclusive in Rust, no actual locking needs to take place -- the mutable borrow statically
    /// guarantees no locks exist. As such, this is a 'zero-cost' operation.
    #[inline(always)]
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }
}

impl<T: ?Sized + fmt::Debug, R> fmt::Debug for Mutex<T, R> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, f)
    }
}

// spin writes `T: ?Sized + Default`, which means the same, since `Default` implies `Sized`.
impl<T: Default, R> Default for Mutex<T, R> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<T, R> From<T> for Mutex<T, R> {
    fn from(data: T) -> Self {
        Self::new(data)
    }
}

impl<'a, T: ?Sized> MutexGuard<'a, T> {
    /// Leak the lock guard, yielding a mutable reference to the underlying data.
    ///
    /// Note that this function will permanently lock the original [`Mutex`].
    #[inline(always)]
    pub fn leak(this: Self) -> &'a mut T {
        InnerMutexGuard::leak(this.inner)
    }
}

impl<'a, T: ?Sized + fmt::Debug> fmt::Debug for MutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'a, T: ?Sized + fmt::Display> fmt::Display for MutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<'a, T: ?Sized> Deref for MutexGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<'a, T: ?Sized> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

#[cfg(feature = "lock_api")]
unsafe impl<R: RelaxStrategy> lock_api_crate::RawMutex for Mutex<(), R> {
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
        self.inner.is_locked()
    }
}
