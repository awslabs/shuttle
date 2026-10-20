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
//! The lock is a single [`BatchSemaphore`] with `PERMITS_ON_INITIALIZATION` permits, where each
//! way of holding the lock is a permit count:
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
//! The semaphore is a [`Fairness::ParkingLot`] one, which behaves like the locks of `parking_lot`
//! (0.12.5, `raw_rwlock.rs`):
//!
//! * `parking_lot` decides whether to grant a new request only from its lock word, and a request
//!   that waits does not hold back other requests. A plain read is refused only while `WRITER_BIT`
//!   is set. The semaphore matches each new request against the free permits, and a request that
//!   waits holds nothing.
//! * A plain unlock wakes the requests that `parking_lot`'s wake filter unparks, from the front of
//!   the queue: the readers up to and including the first writer, and after an upgradable reader,
//!   the plain readers behind the writers and upgradable readers that it passes over. It leaves the
//!   lock free, so a task that does not wait, including the one that unlocked, can take the lock
//!   before they run, and a woken request that finds the lock taken parks again, at the back of the
//!   queue (`parking_lot_lets_a_later_writer_overtake_a_parked_one` in
//!   `tests/rwlock_reference_model.rs` shows this on the real `parking_lot`).
//! * The requests that an unlock leaves parked do not try again until a later unlock wakes them. So
//!   a reader can stay parked while only an upgradable reader holds the lock, which would admit it:
//!   a writer that an unlock woke ahead of it lost the lock to the upgradable reader, and parked
//!   again behind it. If the upgradable reader then waits for that reader, `parking_lot` deadlocks,
//!   and so does this lock (`stranded_reader` in `tests/rwlock_unfair.rs`).
//! * A fair unlock (`unlock_exclusive_fair`, and the `bump_*` methods) hands the lock to the
//!   requests that it wakes instead, inside the release (see [`BatchSemaphore::release_fair`]). In
//!   0.12.5, `unlock_upgradable_fair` is a plain unlock, and the fair unlock and the `bump` of a
//!   read lock, like every shared unlock, wake only a writer that waits for the readers to leave
//!   (see [`BatchSemaphore::release_without_waking`]).
//! * `parking_lot` also makes a plain unlock fair now and then, on a timer. Shuttle has no clock,
//!   so instead a woken request that has been overtaken
//!   [`Config::parking_lot_fairness_bound`](shuttle::Config::parking_lot_fairness_bound) times (2
//!   by default) is handed the lock (see [`Fairness::ParkingLot`]). No loop of unlocks can then
//!   starve a task that waits, but Shuttle leaves out the schedules in which a woken request is
//!   overtaken more often than that, and the deadlocks that need them: a stranded reader whose
//!   writer is passed by two or more unlocks before the upgradable reader overtakes it is found
//!   only with a larger bound.
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
//! An operation that does not block is one semaphore operation with one scheduling point. A `write`
//! or an `upgrade` that waits is scheduled again when it takes its permits, and other tasks run
//! while it waits — but every state they can see is a `parking_lot` lock word (the reservation is
//! `WRITER_BIT`), never an artifact between two separate semaphore operations.
//!
//! `try_write` fails while a request waits for the lock, even if the lock is free, as `parking_lot`'s
//! compare-and-swap of the lock word from zero fails while `PARKED_BIT` is set (see
//! [`BatchSemaphore::try_acquire_without_overtaking`]). The other `try_*` methods, like
//! `parking_lot`'s, do not look at waiting requests. The `bump_*` methods unlock and lock again
//! only while a task waits for the lock (`PARKED_BIT`), as `parking_lot`'s do, and `bump_shared`
//! only while a writer waits for the readers to leave. Otherwise they keep the lock held.
//!
//! `is_locked` and `is_locked_exclusive` are reads of the lock state with one scheduling point and
//! no effect (see [`BatchSemaphore::load_permits`]), like `parking_lot`'s loads of the state word.
//! The `lock_api` defaults would probe with `try_lock_*` and unlock again, which transiently holds
//! the lock: another task's `try_*` could fail against the probe, a state `parking_lot` cannot
//! show. `WRITER_BIT` is exactly "no permits are free": a writer that holds the lock has every
//! permit, and a writer or an upgrade that waits for the readers holds the reservation.
//!
//! # Causality and Shuttle Explorer
//!
//! The semaphore records vector clocks and Explorer events. A task that is granted permits is
//! after the releases of those permits, and a request that an unlock wakes is after the unlock. A
//! `try_*` that fails is after the last successful acquire, as for Shuttle's other semaphore-based
//! locks, and a `try_write` that a waiting request makes fail is after the requests that parked.
//! `PERMITS_ON_INITIALIZATION` is small enough for Explorer (JavaScript) to show the permit counts
//! exactly.
//!
//! # Panics and stopped executions
//!
//! A release that a task makes while it panics, or while Shuttle stops an execution, closes the
//! semaphore, as for every lock that is built on `BatchSemaphore`: that is how Shuttle models lock
//! poisoning. This includes a panic that the task catches and survives, after which the lock stays
//! closed for the rest of the execution. On a closed lock, a `try_*` fails, `is_locked` and
//! `is_locked_exclusive` are true, and a blocking request returns at once without the lock while
//! `std::thread::panicking()` is true, and panics otherwise. All tasks share one OS thread, so
//! `panicking()` is also true in a task that runs while another task is suspended in the middle of
//! unwinding: that task can then get a guard without the lock while another task holds it.
//! `parking_lot` has none of these states, since its locks are not poisoned.
//!
//! # Limits
//!
//! * The fairness bound stands in for `parking_lot`'s timer (see above), so Shuttle explores only
//!   the schedules in which a woken request is overtaken at most that many times. `parking_lot`'s
//!   timer also makes the first unlock after its hash table grows fair, and one after a critical
//!   section of more than a millisecond; Shuttle lets other requests in there too.
//! * In `parking_lot`, `bump_exclusive` and `bump_upgradable` can leave the lock free for an instant
//!   when a thread has set `PARKED_BIT` but not parked yet, so that a third thread gets the lock
//!   during the `bump`. Here a request that cannot take the lock parks in the same step, so that
//!   state does not occur.

use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use std::thread;
use tracing::trace;

/// The permits of the semaphore, all of which are free on a lock that no task holds. No execution
/// has this many readers, and Explorer (JavaScript) shows the number exactly.
const PERMITS_ON_INITIALIZATION: usize = 1 << 30;

/// The permits that an exclusive lock holds: all of them.
const EXCLUSIVE: usize = PERMITS_ON_INITIALIZATION;

/// The permits that an upgradable lock holds: a strict majority, so that two upgradable readers can
/// never hold the lock at once.
const UPGRADABLE: usize = PERMITS_ON_INITIALIZATION / 2 + 1;

/// The permit that a shared lock holds.
const SHARED: usize = 1;

/// A writer reserves the lock (sets `WRITER_BIT`) once this many permits are available. That is
/// the case exactly when no writer or upgradable reader holds the lock: an upgradable reader leaves
/// at most `PERMITS_ON_INITIALIZATION - UPGRADABLE` permits, and a writer none, while plain readers
/// alone would have to number `UPGRADABLE` to leave fewer.
const WRITER_RESERVES_AT: usize = PERMITS_ON_INITIALIZATION - UPGRADABLE + 1;

/// A Shuttle-backed raw reader-writer lock implementing [`lock_api::RawRwLock`] and its upgrade,
/// downgrade, and fair extensions.
#[derive(Debug)]
pub struct RawRwLock {
    /// Coordinates all access. `PERMITS_ON_INITIALIZATION` permits in total; see the module docs
    /// for the permits each lock state holds.
    sem: BatchSemaphore,
}

impl RawRwLock {
    /// `parking_lot`: `state & PARKED_BIT != 0`, which is set while a request waits for the lock
    /// (see `BatchSemaphore::has_waiters`). A load of the lock state, with one scheduling point,
    /// like `is_locked`.
    fn parked(&self) -> bool {
        self.sem.load_permits().is_some() && self.sem.has_waiters()
    }

    /// Block until `acquire` resolves.
    #[inline]
    fn block_on(acquire: shuttle::future::batch_semaphore::Acquire<'_>) {
        shuttle::future::block_on(acquire).unwrap_or_else(|_| {
            // The semaphore is never explicitly closed and is owned exclusively by this lock, so
            // only a release made while a task panicked can have closed it (see "Panics and stopped
            // executions" in the module docs). While a task unwinds, go on without the lock, so that
            // a destructor that locks can finish.
            if !thread::panicking() {
                panic!(
                    "this `RwLock` was closed by an unlock made while a task panicked, as Shuttle \
                     models lock poisoning, and cannot be locked again"
                );
            }
        });
    }
}

// SAFETY: exclusivity is guaranteed because a writer acquires all `EXCLUSIVE` permits of `sem`,
// which cannot succeed while any reader holds a permit, and a reader cannot acquire a permit while a
// writer holds them all.
unsafe impl lock_api::RawRwLock for RawRwLock {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: RawRwLock = RawRwLock {
        sem: BatchSemaphore::const_new(PERMITS_ON_INITIALIZATION, Fairness::ParkingLot),
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

    /// `parking_lot`'s `unlock_shared` wakes only a writer that waits for the readers to leave,
    /// once the last of them leaves.
    unsafe fn unlock_shared(&self) {
        trace!("releasing parking_lot rwlock {:p} (shared)", self);
        self.sem.release_without_waking(SHARED);
    }

    fn lock_exclusive(&self) {
        trace!("acquiring parking_lot rwlock {:p} (exclusive)", self);
        // Step 1 reserves the lock (sets `WRITER_BIT`) once no writer or upgradable reader holds
        // it. Step 2 waits for the readers to leave, and while it waits, the reservation stops new
        // readers.
        Self::block_on(self.sem.acquire_reserving(WRITER_RESERVES_AT, EXCLUSIVE));
        trace!("acquired parking_lot rwlock {:p} (exclusive)", self);
    }

    /// As `parking_lot`'s, this fails while a request waits for the lock (`PARKED_BIT`), even if
    /// the lock is free (see the module docs).
    fn try_lock_exclusive(&self) -> bool {
        self.sem.try_acquire_without_overtaking(EXCLUSIVE).is_ok()
    }

    unsafe fn unlock_exclusive(&self) {
        trace!("releasing parking_lot rwlock {:p} (exclusive)", self);
        self.sem.release(EXCLUSIVE);
    }

    /// `parking_lot`: `state & (READERS_MASK | WRITER_BIT) != 0`. Any permit held means some task
    /// holds the lock, and no free permit means a writer holds the lock or waits for the readers to
    /// leave. One scheduling point and no effect (see the module docs), unlike the `lock_api`
    /// default, which transiently takes the lock.
    fn is_locked(&self) -> bool {
        match self.sem.load_permits() {
            // A closed lock refuses every request, so it never looks free (see "Panics and stopped
            // executions" in the module docs).
            None => true,
            Some(available) => available < PERMITS_ON_INITIALIZATION,
        }
    }

    /// `parking_lot`: `state & WRITER_BIT != 0`, which is set while a writer holds the lock, and
    /// while a `write` or an `upgrade` waits for the readers to leave (the reservation).
    fn is_locked_exclusive(&self) -> bool {
        match self.sem.load_permits() {
            // A closed lock, as in `is_locked`.
            None => true,
            // `load_permits` counts the permits that a request could take, which is none in two
            // cases: a writer holds every permit, or a reservation keeps the free permits for the
            // `write` or `upgrade` that waits for the readers to leave.
            Some(available) => available == 0,
        }
    }
}

// SAFETY: a fair unlock releases the same permits as a plain one. Handing them to the waiting
// requests inside the release, or waking none of them, only decides who gets the lock next.
unsafe impl lock_api::RawRwLockFair for RawRwLock {
    /// As `unlock_shared`, which `parking_lot`'s fair shared unlock is too.
    unsafe fn unlock_shared_fair(&self) {
        trace!("fair-releasing parking_lot rwlock {:p} (shared)", self);
        self.sem.release_without_waking(SHARED);
    }

    unsafe fn unlock_exclusive_fair(&self) {
        trace!("fair-releasing parking_lot rwlock {:p} (exclusive)", self);
        self.sem.release_fair(EXCLUSIVE);
    }

    /// `parking_lot` unlocks and locks again only while `WRITER_BIT` is set, which a reader sees
    /// only while a `write` or an `upgrade` waits for the readers to leave: then the reservation
    /// keeps every free permit (see `is_locked_exclusive`). Otherwise the lock stays held.
    unsafe fn bump_shared(&self) {
        if self.sem.load_permits() == Some(0) {
            // SAFETY: the caller holds a shared lock.
            unsafe { self.unlock_shared_fair() };
            lock_api::RawRwLock::lock_shared(self);
        }
    }

    /// `parking_lot` unlocks and locks again only while `PARKED_BIT` is set, that is, while a request
    /// waits for the lock, and then hands the lock to the requests that the unlock wakes. Otherwise
    /// the lock stays held.
    unsafe fn bump_exclusive(&self) {
        if self.parked() {
            // SAFETY: the caller holds an exclusive lock.
            unsafe { self.unlock_exclusive_fair() };
            lock_api::RawRwLock::lock_exclusive(self);
        }
    }
}

// SAFETY: downgrading only ever releases permits, so it cannot violate exclusivity; the caller
// still holds a shared permit afterwards.
unsafe impl lock_api::RawRwLockDowngrade for RawRwLock {
    unsafe fn downgrade(&self) {
        // Keep one permit, so no writer can slip in during the transition. Like `parking_lot`'s,
        // this wakes the requests that the released permits let in, and hands them nothing.
        trace!("downgrading parking_lot rwlock {:p} (exclusive -> shared)", self);
        self.sem.release(EXCLUSIVE - SHARED);
    }
}

// SAFETY: an upgradable lock holds a strict majority of the permits, so it excludes writers (which
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

// SAFETY: both conversions only *release* permits, keeping at least one, so the lock is never left
// unheld mid-transition and no illegal overlap is possible.
unsafe impl lock_api::RawRwLockUpgradeDowngrade for RawRwLock {
    unsafe fn downgrade_upgradable(&self) {
        // Keep one permit, so no writer can slip in during the transition. Wakes, and hands over
        // nothing, as `downgrade` does.
        trace!("downgrading parking_lot rwlock {:p} (upgradable -> shared)", self);
        self.sem.release(UPGRADABLE - SHARED);
    }

    unsafe fn downgrade_to_upgradable(&self) {
        // We still hold a majority afterwards, which is exactly the upgradable state. This cannot
        // block, matching `parking_lot`'s atomic `WRITER_BIT` -> `ONE_READER | UPGRADABLE_BIT`
        // swap: a task merely *waiting* for an upgradable read holds nothing that could stand in
        // our way. Wakes, and hands over nothing, as `downgrade` does.
        trace!("downgrading parking_lot rwlock {:p} (exclusive -> upgradable)", self);
        self.sem.release(EXCLUSIVE - UPGRADABLE);
    }
}

// SAFETY: as for `RawRwLockFair`.
unsafe impl lock_api::RawRwLockUpgradeFair for RawRwLock {
    /// A plain unlock, as in `parking_lot` 0.12.5, whose `unlock_upgradable_fair` does not force a
    /// fair unlock.
    unsafe fn unlock_upgradable_fair(&self) {
        trace!("fair-releasing parking_lot rwlock {:p} (upgradable)", self);
        self.sem.release(UPGRADABLE);
    }

    /// As `bump_exclusive`: only while a request waits for the lock, and then a fair unlock, as in
    /// `parking_lot`, unlike `unlock_upgradable_fair`.
    unsafe fn bump_upgradable(&self) {
        if self.parked() {
            trace!("bumping parking_lot rwlock {:p} (upgradable)", self);
            self.sem.release_fair(UPGRADABLE);
            lock_api::RawRwLockUpgrade::lock_upgradable(self);
        }
    }
}
