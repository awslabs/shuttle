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

/// Permits held by an upgradable read lock: a strict majority, so that two upgradable readers can
/// never hold the lock at once (see the module docs). This leaves `MAX_READERS - UPGRADABLE_READER`
/// permits (~1.2×10^18) for plain readers to share alongside an upgradable one, which is just as
/// unreachable in a Shuttle execution as `MAX_READERS` itself.
const UPGRADABLE_READER: usize = MAX_READERS / 2 + 1;

/// A Shuttle-backed raw reader-writer lock implementing [`lock_api::RawRwLock`] and its upgrade,
/// downgrade, and fair extensions.
#[derive(Debug)]
pub struct RawRwLock {
    /// Coordinates all access. `MAX_READERS` permits total; see the module docs for the permit count
    /// each lock state holds.
    sem: BatchSemaphore,
}

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

// SAFETY:
// In order for this impl to be safe, we need to ensure:
// * An exclusive lock can't be taken while an exclusive or a shared lock exists.
// * A shared lock can't be taken while an exclusive lock exists.
// * With `send_guard`, the `unlock_*` methods work from another thread than the one that locked.
//
// The lock is one `BatchSemaphore` with `MAX_READERS` permits, which `INIT` makes. A shared lock
// takes 1 permit and an exclusive lock takes all `MAX_READERS`. lock_api gives back what a lock took,
// through the `unlock_*` methods, only for a lock that is held, as their `# Safety` sections ask. So
// the permits that are out are what the held locks took.
// An exclusive lock can be granted only when all `MAX_READERS` permits are free, which is when no
// lock of any kind is held. A shared lock needs one free permit, and while an exclusive lock is held
// none are free. So both properties hold.
// The third property holds as for `RawMutex`: the semaphore doesn't care which Shuttle task gives
// permits back. Also as for `RawMutex`, it holds only for the threads that Shuttle runs. A guard
// that `send_guard` lets safe code move into a thread that Shuttle doesn't run, for example one
// from `std::thread::scope`, uses the semaphore's `RefCell` from that OS thread when it is dropped.
// As for `RawMutex`, the first two properties do not hold once the semaphore is closed: then
// `lock_shared` and `lock_exclusive` return without permits if the thread is panicking (see
// `RawRwLock::acquire`).
unsafe impl lock_api::RawRwLock for RawRwLock {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: RawRwLock = RawRwLock {
        sem: BatchSemaphore::const_new(MAX_READERS, Fairness::StrictlyFair),
    };

    // Gated by `send_guard`; defined once as `crate::GuardMarker` (see `lib.rs`).
    type GuardMarker = crate::GuardMarker;

    fn lock_shared(&self) {
        trace!("acquiring parking_lot rwlock {:p} (shared)", self);
        Self::acquire(self.sem.acquire(1));
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
        Self::acquire(self.sem.acquire(MAX_READERS));
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

// SAFETY:
// In order for this impl to be safe, we need to ensure:
// * The fair unlocks keep the lock exclusive, as the plain ones do.
//
// `unlock_shared_fair` and `unlock_exclusive_fair` give back the same permits as `unlock_shared` and
// `unlock_exclusive`. The semaphore is strictly fair, so a plain release already grants the permits
// to the waiters in queue order. It passes over only an upgradable request that it can't grant yet
// (see `unblock_waiters_from_front` in `BatchSemaphore`). The default `bump_shared` and
// `bump_exclusive` unlock fairly and then lock again, so they keep the lock exclusive too.
unsafe impl lock_api::RawRwLockFair for RawRwLock {
    unsafe fn unlock_shared_fair(&self) {
        self.sem.release(1);
    }

    unsafe fn unlock_exclusive_fair(&self) {
        self.sem.release(MAX_READERS);
    }
}

// SAFETY:
// In order for this impl to be safe, we need to ensure:
// * `downgrade` turns an exclusive lock into a shared one, and no other context can take an
//   exclusive lock in between.
//
// The caller holds an exclusive lock, as the `# Safety` section of `downgrade` asks, so it holds all
// `MAX_READERS` permits. `downgrade` gives back all but one in one release, so the caller then holds
// what a shared lock holds. One permit stays out the whole time, so an exclusive lock, which needs
// all of them, can't be granted in between.
unsafe impl lock_api::RawRwLockDowngrade for RawRwLock {
    unsafe fn downgrade(&self) {
        // Exclusive holds all `MAX_READERS` permits; a shared lock holds 1. Release the difference,
        // keeping one so no writer can slip in during the transition.
        trace!("downgrading parking_lot rwlock {:p} (exclusive -> shared)", self);
        self.sem.release(MAX_READERS - 1);
    }
}

// SAFETY:
// In order for this impl to be safe, we need to ensure:
// * An upgradable lock excludes exclusive locks and other upgradable locks.
// * `upgrade` and `try_upgrade` turn an upgradable lock into an exclusive one, and no other context
//   can take an exclusive or an upgradable lock in between.
//
// An upgradable lock takes `UPGRADABLE_READER` permits, which is more than half of `MAX_READERS`. An
// exclusive lock needs all of them, and a second upgradable lock would need more than half again, so
// neither can be granted while it is held.
// `upgrade` and `try_upgrade` keep the `UPGRADABLE_READER` permits that the caller holds and take the
// rest (see `BatchSemaphore::upgrade` and `BatchSemaphore::try_upgrade`). So the caller holds more
// than half of the permits the whole time, and all of them at the end. The caller holds an
// upgradable lock, as their `# Safety` sections ask.
// As for `RawRwLock`, the properties do not hold once the semaphore is closed: then
// `lock_upgradable` and `upgrade` return without the permits if the thread is panicking.
unsafe impl lock_api::RawRwLockUpgrade for RawRwLock {
    fn lock_upgradable(&self) {
        trace!("acquiring parking_lot rwlock {:p} (upgradable)", self);
        Self::acquire(self.sem.acquire_bypassable(UPGRADABLE_READER));
        trace!("acquired parking_lot rwlock {:p} (upgradable)", self);
    }

    fn try_lock_upgradable(&self) -> bool {
        self.sem.try_acquire(UPGRADABLE_READER).is_ok()
    }

    unsafe fn unlock_upgradable(&self) {
        trace!("releasing parking_lot rwlock {:p} (upgradable)", self);
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

// SAFETY:
// In order for this impl to be safe, we need to ensure:
// * `downgrade_upgradable` turns an upgradable lock into a shared one, and
//   `downgrade_to_upgradable` turns an exclusive lock into an upgradable one.
// * No other context can take a lock that conflicts with the caller's in between.
//
// The caller holds the old lock, as the `# Safety` sections ask. Both methods only give back
// permits, in one release, and keep what the new lock holds: 1 permit for a shared lock, and
// `UPGRADABLE_READER` for an upgradable one. What the caller holds never drops below what the new
// lock needs, so no lock that conflicts with the new one can be granted in between.
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

// SAFETY:
// In order for this impl to be safe, we need to ensure:
// * `unlock_upgradable_fair` keeps the lock exclusive, as `unlock_upgradable` does.
//
// It gives back the same `UPGRADABLE_READER` permits as `unlock_upgradable`. The semaphore is
// strictly fair, so a plain release already grants them to the waiters in queue order, and passes
// over only an upgradable request that it can't grant yet (see `RawRwLockFair` above). The default
// `bump_upgradable` unlocks fairly and then locks again.
unsafe impl lock_api::RawRwLockUpgradeFair for RawRwLock {
    unsafe fn unlock_upgradable_fair(&self) {
        self.sem.release(UPGRADABLE_READER);
    }
}
