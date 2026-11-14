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
//! The lock is a single [`BatchSemaphore`] holding `MAX_READERS` permits, where each way of holding
//! the lock is a permit count:
//!
//! | Lock state     | Permits held         |
//! |----------------|----------------------|
//! | shared         | `1`                  |
//! | upgradable     | `UPGRADABLE_READER`  |
//! | exclusive      | `MAX_READERS`        |
//!
//! An exclusive lock takes *every* permit, so it can only be held when nobody else holds the lock.
//! `UPGRADABLE_READER` is a strict majority of the permits, which makes `parking_lot`'s rules for
//! upgradable reads fall out of the permit arithmetic:
//!
//! * Two upgradable readers cannot coexist (two majorities do not fit), matching `parking_lot`'s
//!   single `UPGRADABLE_BIT`. This is also what makes upgrades deadlock-free: there is never more
//!   than one upgrade in flight.
//! * An upgradable reader excludes a writer (which needs all the permits) but not plain readers,
//!   which still have a majority-minus-one permits to share out.
//!
//! Upgradable acquires allow later requests to pass while waiting for enough permits. A second
//! upgradable reader therefore does not block plain readers while the first still holds the lock.
//!
//! Modelling every lock state as a permit count on *one* semaphore is what keeps the transitions
//! between them atomic, mirroring the single atomic bit-twiddle each one is in `parking_lot`. In
//! particular, taking an upgradable read is one `acquire`, so a task waiting for one holds nothing,
//! and `downgrade_to_upgradable` only *releases* permits, so it can never block.
//!
//! Upgrading is the one transition that has to acquire: it needs the `MAX_READERS -
//! UPGRADABLE_READER` permits it does not already hold. That is done with
//! [`BatchSemaphore::upgrade`], which keeps hold of the permits it has and takes priority over
//! queued waiters, so an upgrade waits only for the plain readers currently in the lock and can
//! never be overtaken by a writer. This is what `parking_lot` guarantees by swapping
//! `ONE_READER | UPGRADABLE_BIT` for `WRITER_BIT` in one atomic step and then waiting for readers to
//! drain: an upgradable reader's view of the data cannot change underneath it while it upgrades.

use shuttle::future::batch_semaphore::{Acquire, BatchSemaphore, Fairness};
use shuttle_engine::runtime::per_execution::ValueLock;
use shuttle_engine::runtime::thread::switch;
use std::thread;
use std::time::{Duration, Instant};
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

/// Permits held by an upgradable read lock: a strict majority, so that two upgradable readers can
/// never hold the lock at once (see the module docs). This leaves `MAX_READERS - UPGRADABLE_READER`
/// permits (~1.2×10^18) for plain readers to share alongside an upgradable one, which is just as
/// unreachable in a Shuttle execution as `MAX_READERS` itself.
const UPGRADABLE_READER: usize = MAX_READERS / 2 + 1;

/// A Shuttle-backed raw reader-writer lock implementing [`lock_api::RawRwLock`] and its upgrade,
/// downgrade, fair, timed and recursive extensions.
///
/// The semaphore's state belongs to the running execution, not to the lock (see
/// [`BatchSemaphore`]), so a `static` lock starts every execution unlocked, with no waiters,
/// whatever the execution before left behind. The value the lock protects belongs to the lock,
/// though, so an execution that takes the lock while a guard of another execution holds the value
/// panics rather than hand out a second reference to it, even if both only read it, as for
/// [`RawMutex`](crate::RawMutex) (see [`ValueLock`]).
#[derive(Debug)]
pub struct RawRwLock {
    /// Coordinates all access. `MAX_READERS` permits total; see the module docs for the permit count
    /// each lock state holds.
    sem: BatchSemaphore,
    /// Keeps the value of the lock to the guards of one execution at a time, which the semaphore,
    /// being per execution, can't (see [`ValueLock`]). Counts guards of every kind; upgrades and
    /// downgrades keep the guard.
    value: ValueLock,
}

/// The name of the lock in the message of [`ValueLock::acquire`].
const KIND: &str = "parking_lot RwLock";

impl RawRwLock {
    #[inline]
    fn acquire(acquire: Acquire<'_>) {
        shuttle::future::block_on(acquire).unwrap_or_else(|_| {
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
        value: ValueLock::new(),
    };

    // Gated by `send_guard`; defined once as `crate::GuardMarker` (see `lib.rs`).
    type GuardMarker = crate::GuardMarker;

    fn lock_shared(&self) {
        trace!("acquiring parking_lot rwlock {:p} (shared)", self);
        Self::acquire(self.sem.acquire(1));
        self.value.acquire(KIND);
        trace!("acquired parking_lot rwlock {:p} (shared)", self);
    }

    fn try_lock_shared(&self) -> bool {
        let acquired = self.sem.try_acquire(1).is_ok();
        if acquired {
            self.value.acquire(KIND);
        }
        acquired
    }

    unsafe fn unlock_shared(&self) {
        trace!("releasing parking_lot rwlock {:p} (shared)", self);
        self.value.release();
        self.sem.release(1);
    }

    fn lock_exclusive(&self) {
        trace!("acquiring parking_lot rwlock {:p} (exclusive)", self);
        Self::acquire(self.sem.acquire(MAX_READERS));
        self.value.acquire(KIND);
        trace!("acquired parking_lot rwlock {:p} (exclusive)", self);
    }

    fn try_lock_exclusive(&self) -> bool {
        let acquired = self.sem.try_acquire(MAX_READERS).is_ok();
        if acquired {
            self.value.acquire(KIND);
        }
        acquired
    }

    unsafe fn unlock_exclusive(&self) {
        trace!("releasing parking_lot rwlock {:p} (exclusive)", self);
        self.value.release();
        self.sem.release(MAX_READERS);
    }

    // `lock_api`'s defaults take and release the lock (`is_locked` exclusively, `is_locked_exclusive`
    // shared), so a task asking would make another task's `try_*` fail, which can't happen with
    // `parking_lot`, where these are loads. Here they are loads too, after a scheduling point, as an
    // atomic load is in Shuttle. Only a writer holds every permit (see the module docs).
    fn is_locked(&self) -> bool {
        switch();
        self.sem.available_permits() < MAX_READERS
    }

    fn is_locked_exclusive(&self) -> bool {
        switch();
        self.sem.available_permits() == 0
    }
}

// Safety: Shuttle's semaphore is strictly fair, so a plain `release` already hands permits to the
// next eligible waiter in FIFO order. Fair unlocking is therefore identical to a normal unlock.
unsafe impl lock_api::RawRwLockFair for RawRwLock {
    unsafe fn unlock_shared_fair(&self) {
        self.value.release();
        self.sem.release(1);
    }

    unsafe fn unlock_exclusive_fair(&self) {
        self.value.release();
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

// Safety: an upgradable lock holds a strict majority of the permits, so it excludes writers (which
// need all of them) and other upgradable readers (which would need another majority), while still
// permitting plain shared readers.
unsafe impl lock_api::RawRwLockUpgrade for RawRwLock {
    fn lock_upgradable(&self) {
        trace!("acquiring parking_lot rwlock {:p} (upgradable)", self);
        Self::acquire(self.sem.acquire_bypassable(UPGRADABLE_READER));
        self.value.acquire(KIND);
        trace!("acquired parking_lot rwlock {:p} (upgradable)", self);
    }

    fn try_lock_upgradable(&self) -> bool {
        let acquired = self.sem.try_acquire(UPGRADABLE_READER).is_ok();
        if acquired {
            self.value.acquire(KIND);
        }
        acquired
    }

    unsafe fn unlock_upgradable(&self) {
        trace!("releasing parking_lot rwlock {:p} (upgradable)", self);
        self.value.release();
        self.sem.release(UPGRADABLE_READER);
    }

    unsafe fn upgrade(&self) {
        trace!("upgrading parking_lot rwlock {:p} (upgradable -> exclusive)", self);
        // Keep the `UPGRADABLE_READER` permits we hold and take the rest. `BatchSemaphore::upgrade`
        // never lets go of what we hold, and takes priority over queued waiters, so no writer can be
        // granted the lock part-way through the upgrade; we wait only for the plain readers that are
        // currently in the lock. The returned future must be driven to completion via `block_on`.
        shuttle::future::block_on(self.sem.upgrade(UPGRADABLE_READER, MAX_READERS)).unwrap_or_else(|_| {
            if !thread::panicking() {
                unreachable!()
            }
        });
    }

    unsafe fn try_upgrade(&self) -> bool {
        // As `upgrade`, but only if the permits held by plain readers are free right now. Queued
        // waiters do not make this fail: like `parking_lot`, which only inspects `READERS_MASK` in
        // `try_upgrade_slow`, we already hold the lock in a mode no writer can be holding.
        self.sem.try_upgrade(UPGRADABLE_READER, MAX_READERS).is_ok()
    }
}

// Safety: both conversions only *release* permits, keeping at least one, so the lock is never left
// unheld mid-transition and no illegal overlap is possible.
unsafe impl lock_api::RawRwLockUpgradeDowngrade for RawRwLock {
    unsafe fn downgrade_upgradable(&self) {
        // Upgradable (`UPGRADABLE_READER`) -> shared (1). Keep one permit so no writer can slip in
        // during the transition.
        trace!("downgrading parking_lot rwlock {:p} (upgradable -> shared)", self);
        self.sem.release(UPGRADABLE_READER - 1);
    }

    unsafe fn downgrade_to_upgradable(&self) {
        // Exclusive (all permits) -> upgradable (`UPGRADABLE_READER`). Releasing the difference is
        // enough: we still hold a majority, which is exactly the upgradable state. Notably this
        // cannot block, matching `parking_lot`'s atomic `WRITER_BIT` -> `ONE_READER |
        // UPGRADABLE_BIT` swap -- a task merely *waiting* for an upgradable read holds nothing that
        // could stand in our way.
        trace!("downgrading parking_lot rwlock {:p} (exclusive -> upgradable)", self);
        self.sem.release(MAX_READERS - UPGRADABLE_READER);
    }
}

// Safety: fair unlocking is identical to normal unlocking for a strictly fair semaphore.
unsafe impl lock_api::RawRwLockUpgradeFair for RawRwLock {
    unsafe fn unlock_upgradable_fair(&self) {
        self.value.release();
        self.sem.release(UPGRADABLE_READER);
    }
}

// Safety: a recursive read takes one permit, as a read does, so it is as exclusive as `lock_shared`.
//
// `parking_lot` grants a recursive read at once while any reader holds the lock, even with a writer
// waiting: that is what lets a thread that already reads take another read without deadlocking
// behind the writer. `BatchSemaphore::try_upgrade` is the semaphore's one way to take permits ahead
// of queued waiters, and `try_upgrade(1, 2)` asks for the one permit a read needs (the semaphore
// doesn't check the permits a caller says it holds). It succeeds unless a writer holds the lock; a
// queued writer or a pending upgrade doesn't stop it, as they don't in `parking_lot` while readers
// hold the lock. When a writer holds it, the read queues like any other.
unsafe impl lock_api::RawRwLockRecursive for RawRwLock {
    fn lock_shared_recursive(&self) {
        trace!("acquiring parking_lot rwlock {:p} (shared, recursive)", self);
        if !self.try_lock_shared_recursive() {
            lock_api::RawRwLock::lock_shared(self);
        }
    }

    fn try_lock_shared_recursive(&self) -> bool {
        let acquired = self.sem.try_upgrade(1, 2).is_ok();
        if acquired {
            self.value.acquire(KIND);
        }
        acquired
    }
}

// Safety: a timed acquire is an acquire that never times out (see below), so it is as exclusive as
// the untimed one.
//
// Shuttle has no clock that a timeout could be measured against, so a timed acquire waits until it
// gets the lock and returns `true`, as Shuttle's `Condvar::wait_timeout` never times out. A task that
// would time out in a real run blocks instead; if nothing ever releases the lock, Shuttle reports a
// deadlock.
unsafe impl lock_api::RawRwLockTimed for RawRwLock {
    type Duration = Duration;
    type Instant = Instant;

    fn try_lock_shared_for(&self, _timeout: Duration) -> bool {
        lock_api::RawRwLock::lock_shared(self);
        true
    }

    fn try_lock_shared_until(&self, _timeout: Instant) -> bool {
        lock_api::RawRwLock::lock_shared(self);
        true
    }

    fn try_lock_exclusive_for(&self, _timeout: Duration) -> bool {
        lock_api::RawRwLock::lock_exclusive(self);
        true
    }

    fn try_lock_exclusive_until(&self, _timeout: Instant) -> bool {
        lock_api::RawRwLock::lock_exclusive(self);
        true
    }
}

// Safety: as for `RawRwLockTimed` and `RawRwLockRecursive`.
unsafe impl lock_api::RawRwLockRecursiveTimed for RawRwLock {
    fn try_lock_shared_recursive_for(&self, _timeout: Duration) -> bool {
        lock_api::RawRwLockRecursive::lock_shared_recursive(self);
        true
    }

    fn try_lock_shared_recursive_until(&self, _timeout: Instant) -> bool {
        lock_api::RawRwLockRecursive::lock_shared_recursive(self);
        true
    }
}

// Safety: as for `RawRwLockTimed` and `RawRwLockUpgrade`.
unsafe impl lock_api::RawRwLockUpgradeTimed for RawRwLock {
    fn try_lock_upgradable_for(&self, _timeout: Duration) -> bool {
        lock_api::RawRwLockUpgrade::lock_upgradable(self);
        true
    }

    fn try_lock_upgradable_until(&self, _timeout: Instant) -> bool {
        lock_api::RawRwLockUpgrade::lock_upgradable(self);
        true
    }

    unsafe fn try_upgrade_for(&self, _timeout: Duration) -> bool {
        // Safety: the caller holds an upgradable lock, as `try_upgrade_for` requires.
        unsafe { lock_api::RawRwLockUpgrade::upgrade(self) };
        true
    }

    unsafe fn try_upgrade_until(&self, _timeout: Instant) -> bool {
        // Safety: as above.
        unsafe { lock_api::RawRwLockUpgrade::upgrade(self) };
        true
    }
}
