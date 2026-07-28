//! The Shuttle-backed raw reader-writer lock that underpins [`crate::RwLock`].
//!
//! This provides [`RawRwLock`], an implementation of [`lock_api::RawRwLock`] and its upgrade,
//! downgrade, and fair extensions, routed through Shuttle's scheduler. The user-facing `RwLock`,
//! guards, and `Arc`-based guards are the generic `lock_api` types specialised to this raw lock
//! (see [`crate::RwLock`]), exactly how the real `parking_lot` crate layers its `RwLock` on top of
//! `lock_api`.
//!
//! # Modelling
//!
//! The lock is modelled with two [`BatchSemaphore`]s:
//!
//! * `sem` holds `MAX_READERS` permits. A shared (read) lock takes a single permit; an exclusive
//!   (write) lock takes *all* `MAX_READERS` permits, so it can only be held when no readers are
//!   present.
//! * `upgradable_sem` holds a single permit and is taken by an upgradable read lock, guaranteeing
//!   there is at most one upgradable reader at a time (a requirement for deadlock-free upgrades). An
//!   upgradable reader also takes one permit from `sem`, so it behaves like a shared reader that can
//!   later escalate to a writer.

use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use std::thread;
use tracing::trace;

/// Sentinel permit count representing "all readers". An exclusive (write) lock is modelled by
/// acquiring all `MAX_READERS` permits, so it can only be taken when no reader holds one; a shared
/// (read) lock takes a single permit. This also bounds the number of concurrent readers, but the
/// bound (~2.3×10^18 on a 64-bit target) is unreachable in any Shuttle execution.
///
/// Shuttle's `BatchSemaphore` stores its permit count as a plain `usize` (no reserved bits, unlike
/// tokio's native `Semaphore`), and correct lock pairing keeps the available count within
/// `[0, MAX_READERS]`, so this value cannot overflow the semaphore's accounting. `usize::MAX >> 3`
/// leaves an 8x defensive margin while staying close to `parking_lot`'s own large reader capacity.
const MAX_READERS: usize = usize::MAX >> 3;

/// A Shuttle-backed raw reader-writer lock implementing [`lock_api::RawRwLock`] and its upgrade,
/// downgrade, and fair extensions.
#[derive(Debug)]
pub struct RawRwLock {
    /// Coordinates read/write access. `MAX_READERS` permits total; a read takes 1, a write takes all.
    sem: BatchSemaphore,
    /// Ensures at most one upgradable reader exists at a time.
    upgradable_sem: BatchSemaphore,
}

impl RawRwLock {
    #[inline]
    fn acquire(sem: &BatchSemaphore, permits: usize) {
        sem.acquire_blocking(permits).unwrap_or_else(|_| {
            // The semaphores are never explicitly closed and are owned exclusively by this lock, so
            // a closed semaphore here can only be observed while unwinding from a panic.
            if !thread::panicking() {
                unreachable!()
            }
        });
    }
}

// Safety: exclusivity is guaranteed because a writer acquires all `MAX_READERS` permits of `sem`,
// which cannot succeed while any reader holds a permit, and a reader cannot acquire a permit while a
// writer holds them all.
unsafe impl lock_api::RawRwLock for RawRwLock {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: RawRwLock = RawRwLock {
        sem: BatchSemaphore::const_new(MAX_READERS, Fairness::StrictlyFair),
        upgradable_sem: BatchSemaphore::const_new(1, Fairness::StrictlyFair),
    };

    // Gated by `send_guard`; defined once as `crate::GuardMarker` (see `lib.rs`).
    type GuardMarker = crate::GuardMarker;

    fn lock_shared(&self) {
        trace!("acquiring parking_lot rwlock {:p} (shared)", self);
        Self::acquire(&self.sem, 1);
        trace!("acquired parking_lot rwlock {:p} (shared)", self);
    }

    fn try_lock_shared(&self) -> bool {
        self.sem.try_acquire(1).is_ok()
    }

    unsafe fn unlock_shared(&self) {
        trace!("releasing parking_lot rwlock {:p} (shared)", self);
        self.sem.release(1);
    }

    fn lock_exclusive(&self) {
        trace!("acquiring parking_lot rwlock {:p} (exclusive)", self);
        Self::acquire(&self.sem, MAX_READERS);
        trace!("acquired parking_lot rwlock {:p} (exclusive)", self);
    }

    fn try_lock_exclusive(&self) -> bool {
        self.sem.try_acquire(MAX_READERS).is_ok()
    }

    unsafe fn unlock_exclusive(&self) {
        trace!("releasing parking_lot rwlock {:p} (exclusive)", self);
        self.sem.release(MAX_READERS);
    }
}

// Safety: Shuttle's semaphore is strictly fair, so a plain `release` already hands permits to the
// next waiter in FIFO order. Fair unlocking is therefore identical to a normal unlock.
unsafe impl lock_api::RawRwLockFair for RawRwLock {
    unsafe fn unlock_shared_fair(&self) {
        self.sem.release(1);
    }

    unsafe fn unlock_exclusive_fair(&self) {
        self.sem.release(MAX_READERS);
    }
}

// Safety: downgrading only ever releases permits, so it cannot violate exclusivity; the caller
// still holds a shared permit afterwards.
unsafe impl lock_api::RawRwLockDowngrade for RawRwLock {
    unsafe fn downgrade(&self) {
        // Exclusive holds all `MAX_READERS` permits; a shared lock holds 1. Release the difference,
        // keeping one so no writer can slip in during the transition.
        trace!("downgrading parking_lot rwlock {:p} (exclusive -> shared)", self);
        self.sem.release(MAX_READERS - 1);
    }
}

// Safety: an upgradable lock holds exactly one `sem` permit plus the single `upgradable_sem` permit,
// so it excludes writers and other upgradable readers while still permitting plain shared readers.
unsafe impl lock_api::RawRwLockUpgrade for RawRwLock {
    fn lock_upgradable(&self) {
        trace!("acquiring parking_lot rwlock {:p} (upgradable)", self);
        // Take the upgradable slot first, then a shared permit. Ordering matters so that a failed
        // acquisition never leaves the upgradable slot held while blocking on `sem`.
        Self::acquire(&self.upgradable_sem, 1);
        Self::acquire(&self.sem, 1);
        trace!("acquired parking_lot rwlock {:p} (upgradable)", self);
    }

    fn try_lock_upgradable(&self) -> bool {
        if self.upgradable_sem.try_acquire(1).is_err() {
            return false;
        }
        if self.sem.try_acquire(1).is_err() {
            // Roll back the upgradable slot so we don't leak it.
            self.upgradable_sem.release(1);
            return false;
        }
        true
    }

    unsafe fn unlock_upgradable(&self) {
        trace!("releasing parking_lot rwlock {:p} (upgradable)", self);
        self.sem.release(1);
        self.upgradable_sem.release(1);
    }

    unsafe fn upgrade(&self) {
        trace!("upgrading parking_lot rwlock {:p} (upgradable -> exclusive)", self);
        // We currently hold 1 `sem` permit; acquire the remaining `MAX_READERS - 1`. Using the
        // semaphore's `upgrade` (rather than a naive acquire) preserves FIFO ordering and avoids the
        // deadlock where the upgrader would block on the very permit it already holds. The returned
        // future must be driven to completion via `block_on`.
        shuttle::future::block_on(self.sem.upgrade(1, MAX_READERS)).unwrap_or_else(|_| {
            if !thread::panicking() {
                unreachable!()
            }
        });
        // Now an exclusive writer; release the upgradable slot so future upgradable readers may
        // queue (they will still block on `sem` until we release exclusive access).
        self.upgradable_sem.release(1);
    }

    unsafe fn try_upgrade(&self) -> bool {
        // We hold 1 `sem` permit; grabbing the remaining `MAX_READERS - 1` yields exclusive access.
        if self.sem.try_acquire(MAX_READERS - 1).is_ok() {
            self.upgradable_sem.release(1);
            true
        } else {
            false
        }
    }
}

// Safety: these conversions preserve the invariant that a writer always holds all permits and an
// upgradable/shared reader holds at least one, so no illegal overlap is possible mid-transition.
unsafe impl lock_api::RawRwLockUpgradeDowngrade for RawRwLock {
    unsafe fn downgrade_upgradable(&self) {
        // Upgradable (1 `sem` + upgradable slot) -> shared (1 `sem`). Just drop the upgradable slot.
        trace!("downgrading parking_lot rwlock {:p} (upgradable -> shared)", self);
        self.upgradable_sem.release(1);
    }

    unsafe fn downgrade_to_upgradable(&self) {
        // Exclusive (all permits) -> upgradable (1 `sem` + upgradable slot). While exclusive, no
        // other upgradable reader can exist, so the upgradable slot is free and acquiring it cannot
        // block. Take it first, then release down to a single `sem` permit.
        trace!("downgrading parking_lot rwlock {:p} (exclusive -> upgradable)", self);
        Self::acquire(&self.upgradable_sem, 1);
        self.sem.release(MAX_READERS - 1);
    }
}

// Safety: fair unlocking is identical to normal unlocking for a strictly fair semaphore.
unsafe impl lock_api::RawRwLockUpgradeFair for RawRwLock {
    unsafe fn unlock_upgradable_fair(&self) {
        self.sem.release(1);
        self.upgradable_sem.release(1);
    }
}
