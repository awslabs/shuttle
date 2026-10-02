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
//! The lock is a single unfair [`BatchSemaphore`] holding `EXCLUSIVE` permits, where each way of
//! holding the lock is a permit count:
//!
//! | Lock state     | Permits held |
//! |----------------|--------------|
//! | shared         | `SHARED`     |
//! | upgradable     | `UPGRADABLE` |
//! | exclusive      | `EXCLUSIVE`  |
//!
//! `UPGRADABLE` is a strict majority of the permits, so two upgradable readers cannot hold the
//! lock at once (`parking_lot`'s single `UPGRADABLE_BIT`), and an upgradable reader excludes
//! writers (which need every permit) but not plain readers.
//!
//! `parking_lot` (0.12.5, `raw_rwlock.rs`) decides who gets the lock only from its lock word, and
//! a request that waits does not hold back other requests. A plain read is refused only while
//! `WRITER_BIT` is set. An unfair semaphore behaves the same way: a request that waits holds
//! nothing, and after a release, the scheduler can grant any request that the free permits let
//! in, in any order.
//!
//! A permit count alone cannot express `WRITER_BIT` while a writer waits for the readers to
//! leave. A writer and an upgrade therefore *reserve* the semaphore (see
//! [`BatchSemaphore::acquire_reserving`]). While a reservation lasts, no other request can take a
//! permit, and the reserving task takes its permits as soon as the readers have left:
//!
//! * A writer reserves the lock once `WRITER_RESERVES_AT` permits are available, which is when no
//!   writer or upgradable reader holds the lock. This is step 1 of `parking_lot`'s
//!   `lock_exclusive_slow`, which sets `WRITER_BIT` only when `WRITER_BIT` and `UPGRADABLE_BIT` are
//!   clear. Until then, the writer holds nothing and stops no reader.
//! * An upgrade reserves the lock at once, as `parking_lot` swaps `ONE_READER | UPGRADABLE_BIT` for
//!   `WRITER_BIT` in one step. It keeps the `UPGRADABLE` permits it holds (see
//!   [`BatchSemaphore::upgrade`]), so no writer can get in during the upgrade.
//!
//! Each operation is one semaphore operation, so it has one scheduling point, and no other task
//! can see a state between two parts of it.
//!
//! # Causality and Shuttle Explorer
//!
//! The semaphore records vector clocks and Explorer events. A task that is granted permits is
//! after the releases of those permits. A `try_*` that fails is after the last successful
//! acquire, as for Shuttle's other semaphore-based locks. `EXCLUSIVE` is small enough for Explorer
//! (JavaScript) to show the permit counts exactly.
//!
//! # Stopped executions
//!
//! While Shuttle stops an execution, for example after a panic, the first release closes the
//! semaphore, as for every lock that is built on `BatchSemaphore`. After that, a `try_*` fails,
//! and a blocking request returns at once without the lock if the task is unwinding, and panics
//! otherwise.
//!
//! # Limits
//!
//! * `parking_lot`'s `try_write` also fails while `PARKED_BIT` is set on a free lock. This happens
//!   after an unlock that wakes some, but not all, of the parked tasks, so it needs at least two
//!   parked tasks besides the task that calls `try_write`. This model does not track parked tasks,
//!   so there its `try_write` can succeed.
//! * A fair unlock hands the lock to the parked tasks in `parking_lot`. Here it is a normal unlock,
//!   after which any request can take the lock. This allows more schedules than `parking_lot`, so
//!   it hides no bug, but a test that relies on the hand-off can fail.

use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use std::thread;
use tracing::trace;

/// The permits of the semaphore, all of which an exclusive lock holds. No execution has this many
/// readers, and Explorer (JavaScript) shows the number exactly.
const EXCLUSIVE: usize = 1 << 30;

/// The permits that an upgradable lock holds: a strict majority of `EXCLUSIVE`, so that two
/// upgradable readers can never hold the lock at once.
const UPGRADABLE: usize = EXCLUSIVE / 2 + 1;

/// The permit that a shared lock holds.
const SHARED: usize = 1;

/// A writer reserves the lock (sets `WRITER_BIT`) once this many permits are available. That is
/// the case exactly when no writer or upgradable reader holds the lock: an upgradable reader leaves
/// at most `EXCLUSIVE - UPGRADABLE` permits, and a writer none, while plain readers alone would
/// have to number `UPGRADABLE` to leave fewer.
const WRITER_RESERVES_AT: usize = EXCLUSIVE - UPGRADABLE + 1;

/// A Shuttle-backed raw reader-writer lock implementing [`lock_api::RawRwLock`] and its upgrade,
/// downgrade, and fair extensions.
#[derive(Debug)]
pub struct RawRwLock {
    /// Coordinates all access. `EXCLUSIVE` permits in total; see the module docs for the permits
    /// each lock state holds.
    sem: BatchSemaphore,
}

impl RawRwLock {
    /// Block until `acquire` resolves.
    #[inline]
    fn block_on(acquire: shuttle::future::batch_semaphore::Acquire<'_>) {
        shuttle::future::block_on(acquire).unwrap_or_else(|_| {
            // The semaphore is never explicitly closed and is owned exclusively by this lock, so a
            // closed semaphore here can only be observed while unwinding from a panic.
            if !thread::panicking() {
                unreachable!()
            }
        });
    }
}

// Safety: exclusivity is guaranteed because a writer acquires all `EXCLUSIVE` permits of `sem`,
// which cannot succeed while any reader holds a permit, and a reader cannot acquire a permit while a
// writer holds them all.
unsafe impl lock_api::RawRwLock for RawRwLock {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: RawRwLock = RawRwLock {
        sem: BatchSemaphore::const_new(EXCLUSIVE, Fairness::Unfair),
    };

    // Gated by `send_guard`; defined once as `crate::GuardMarker` (see `lib.rs`).
    type GuardMarker = crate::GuardMarker;

    fn lock_shared(&self) {
        trace!("acquiring parking_lot rwlock {:p} (shared)", self);
        Self::block_on(self.sem.acquire(SHARED));
        trace!("acquired parking_lot rwlock {:p} (shared)", self);
    }

    fn try_lock_shared(&self) -> bool {
        self.sem.try_acquire(SHARED).is_ok()
    }

    unsafe fn unlock_shared(&self) {
        trace!("releasing parking_lot rwlock {:p} (shared)", self);
        self.sem.release(SHARED);
    }

    fn lock_exclusive(&self) {
        trace!("acquiring parking_lot rwlock {:p} (exclusive)", self);
        // Step 1 reserves the lock (sets `WRITER_BIT`) once no writer or upgradable reader holds
        // it. Step 2 waits for the readers to leave, and while it waits, the reservation stops new
        // readers.
        Self::block_on(self.sem.acquire_reserving(WRITER_RESERVES_AT, EXCLUSIVE));
        trace!("acquired parking_lot rwlock {:p} (exclusive)", self);
    }

    /// Unlike `parking_lot`, this can succeed while `PARKED_BIT` would be set (see the module docs).
    fn try_lock_exclusive(&self) -> bool {
        self.sem.try_acquire(EXCLUSIVE).is_ok()
    }

    unsafe fn unlock_exclusive(&self) {
        trace!("releasing parking_lot rwlock {:p} (exclusive)", self);
        self.sem.release(EXCLUSIVE);
    }
}

// Safety: a fair unlock releases the lock exactly as a normal unlock does. It does not hand the lock
// to a waiting task, as `parking_lot`'s does (see the module docs).
unsafe impl lock_api::RawRwLockFair for RawRwLock {
    unsafe fn unlock_shared_fair(&self) {
        unsafe { lock_api::RawRwLock::unlock_shared(self) }
    }

    unsafe fn unlock_exclusive_fair(&self) {
        unsafe { lock_api::RawRwLock::unlock_exclusive(self) }
    }
}

// Safety: downgrading only ever releases permits, so it cannot violate exclusivity; the caller
// still holds a shared permit afterwards.
unsafe impl lock_api::RawRwLockDowngrade for RawRwLock {
    unsafe fn downgrade(&self) {
        // Keep one permit, so no writer can slip in during the transition.
        trace!("downgrading parking_lot rwlock {:p} (exclusive -> shared)", self);
        self.sem.release(EXCLUSIVE - SHARED);
    }
}

// Safety: an upgradable lock holds a strict majority of the permits, so it excludes writers (which
// need all of them) and other upgradable readers (which would need another majority), while still
// permitting plain shared readers.
unsafe impl lock_api::RawRwLockUpgrade for RawRwLock {
    fn lock_upgradable(&self) {
        trace!("acquiring parking_lot rwlock {:p} (upgradable)", self);
        Self::block_on(self.sem.acquire(UPGRADABLE));
        trace!("acquired parking_lot rwlock {:p} (upgradable)", self);
    }

    fn try_lock_upgradable(&self) -> bool {
        self.sem.try_acquire(UPGRADABLE).is_ok()
    }

    unsafe fn unlock_upgradable(&self) {
        trace!("releasing parking_lot rwlock {:p} (upgradable)", self);
        self.sem.release(UPGRADABLE);
    }

    unsafe fn upgrade(&self) {
        trace!("upgrading parking_lot rwlock {:p} (upgradable -> exclusive)", self);
        // Keep the `UPGRADABLE` permits we hold and reserve the rest. No writer can be granted the
        // lock part-way through the upgrade, and new readers wait; we wait only for the plain
        // readers that are in the lock now. The returned future must be driven to completion.
        Self::block_on(self.sem.upgrade(UPGRADABLE, EXCLUSIVE));
    }

    unsafe fn try_upgrade(&self) -> bool {
        // As `upgrade`, but only if no plain reader holds the lock right now. Like `parking_lot`,
        // which only inspects `READERS_MASK` in `try_upgrade_slow`, waiting requests do not make
        // this fail.
        self.sem.try_upgrade(UPGRADABLE, EXCLUSIVE).is_ok()
    }
}

// Safety: both conversions only *release* permits, keeping at least one, so the lock is never left
// unheld mid-transition and no illegal overlap is possible.
unsafe impl lock_api::RawRwLockUpgradeDowngrade for RawRwLock {
    unsafe fn downgrade_upgradable(&self) {
        // Keep one permit, so no writer can slip in during the transition.
        trace!("downgrading parking_lot rwlock {:p} (upgradable -> shared)", self);
        self.sem.release(UPGRADABLE - SHARED);
    }

    unsafe fn downgrade_to_upgradable(&self) {
        // We still hold a majority afterwards, which is exactly the upgradable state. This cannot
        // block, matching `parking_lot`'s atomic `WRITER_BIT` -> `ONE_READER | UPGRADABLE_BIT`
        // swap: a task merely *waiting* for an upgradable read holds nothing that could stand in
        // our way.
        trace!("downgrading parking_lot rwlock {:p} (exclusive -> upgradable)", self);
        self.sem.release(EXCLUSIVE - UPGRADABLE);
    }
}

// Safety: as for `RawRwLockFair`.
unsafe impl lock_api::RawRwLockUpgradeFair for RawRwLock {
    unsafe fn unlock_upgradable_fair(&self) {
        unsafe { lock_api::RawRwLockUpgrade::unlock_upgradable(self) }
    }
}
