//! The Shuttle-backed raw fair mutex that underpins [`crate::FairMutex`].
//!
//! As in `parking_lot`, [`RawFairMutex`] is a [`RawMutex`] whose every unlock is a fair unlock. A
//! fair unlock hands the lock to the task that has waited longest, which is what Shuttle's
//! [`RawMutex`] does on every unlock (its semaphore is strictly fair), so the two behave the same
//! under Shuttle. They differ in real runs: `parking_lot`'s `Mutex` lets a running thread take the
//! lock ahead of a woken waiter, which Shuttle's `Mutex` doesn't model.

use crate::raw_mutex::RawMutex;
use lock_api::RawMutexFair;

/// A Shuttle-backed raw fair mutex implementing [`lock_api::RawMutex`]. Mirrors
/// `parking_lot::RawFairMutex`.
pub struct RawFairMutex(RawMutex);

// Safety: forwards to `RawMutex`, which is exclusive.
unsafe impl lock_api::RawMutex for RawFairMutex {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = RawFairMutex(<RawMutex as lock_api::RawMutex>::INIT);

    type GuardMarker = <RawMutex as lock_api::RawMutex>::GuardMarker;

    fn lock(&self) {
        self.0.lock()
    }

    fn try_lock(&self) -> bool {
        self.0.try_lock()
    }

    unsafe fn unlock(&self) {
        // Safety: the caller holds the lock, as `unlock` requires.
        unsafe { self.unlock_fair() }
    }

    fn is_locked(&self) -> bool {
        self.0.is_locked()
    }
}

// Safety: forwards to `RawMutex`.
unsafe impl lock_api::RawMutexFair for RawFairMutex {
    unsafe fn unlock_fair(&self) {
        // Safety: the caller holds the lock, as `unlock_fair` requires.
        unsafe { self.0.unlock_fair() }
    }

    unsafe fn bump(&self) {
        // Safety: the caller holds the lock, as `bump` requires.
        unsafe { self.0.bump() }
    }
}

// Safety: forwards to `RawMutex`, whose timed locks never time out.
unsafe impl lock_api::RawMutexTimed for RawFairMutex {
    type Duration = <RawMutex as lock_api::RawMutexTimed>::Duration;
    type Instant = <RawMutex as lock_api::RawMutexTimed>::Instant;

    fn try_lock_until(&self, timeout: Self::Instant) -> bool {
        self.0.try_lock_until(timeout)
    }

    fn try_lock_for(&self, timeout: Self::Duration) -> bool {
        self.0.try_lock_for(timeout)
    }
}
