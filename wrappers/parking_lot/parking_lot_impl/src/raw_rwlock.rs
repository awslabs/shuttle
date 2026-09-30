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
//! The lock keeps the three parts of `parking_lot`'s lock word that decide who can take the lock
//! (`parking_lot` 0.12.5, `raw_rwlock.rs`):
//!
//! * `writer` is `WRITER_BIT`. A writer sets it when it has passed step 1 of `lock_exclusive_slow`,
//!   and keeps it while it waits for the readers to leave. An upgrade sets it at once.
//! * `upgradable` is `UPGRADABLE_BIT`.
//! * `readers` is the reader count. An upgradable reader counts as one reader.
//!
//! Each request applies the same checks as `parking_lot`:
//!
//! | Request            | Granted when                             |
//! |--------------------|------------------------------------------|
//! | `read`             | `!writer`                                |
//! | `upgradable_read`  | `!writer && !upgradable`                 |
//! | `write`, step 1    | `!writer && !upgradable`, then set `writer` |
//! | `write`, step 2    | `readers == 0`                           |
//! | `upgrade`          | set `writer` at once, then `readers == 0` |
//! | `try_upgrade`      | `readers == 1`                           |
//!
//! Each operation has one scheduling point, before it reads the state. It then checks and changes
//! the state in one step, so no other task can see a state between two parts of one operation. A
//! request that cannot be granted takes nothing, except a writer after step 1. It waits until an
//! unlock or a downgrade makes its check pass, and then checks again when it runs. So the scheduler
//! can grant any request that `parking_lot`'s checks allow, in any order. Queued waiters do not block
//! other requests, except through the bits above: a waiting writer blocks new readers only after
//! step 1, as in `parking_lot`.
//!
//! # Causality
//!
//! The lock keeps the join of the vector clocks of all its releases: the unlocks and the three
//! downgrades. A task that is granted a request (including `upgrade` and a successful `try_*`)
//! joins that clock into its own. So each acquire happens after every earlier release, which
//! includes the guarantee that a `RwLock` gives: an unlock happens before each later lock that it
//! lets in. It also adds some edges that `parking_lot` does not promise, for example from one
//! reader's unlock to a later reader's lock. Shuttle's semaphore-based locks add the same edges. A
//! `try_*` that fails joins nothing, because `parking_lot`'s failed compare-exchange is `Relaxed`.
//!
//! # Shuttle Explorer
//!
//! Explorer shows only semaphore events, so the lock reports itself as a semaphore of `EXCLUSIVE`
//! permits. A shared lock holds `SHARED` (one) permit, an upgradable lock holds `UPGRADABLE`
//! permits (a strict majority, so two do not fit), and an exclusive lock holds all of them. An
//! upgrade or a downgrade reports the difference. A request that is granted at once reports
//! `SemaphoreAcquireFast`. A request that waits reports `SemaphoreAcquireBlocked`, and later
//! `SemaphoreAcquireUnblocked`, from the waiting task, when the request is granted.
//!
//! # Stopped executions
//!
//! While Shuttle stops an execution, for example after a panic, the first release closes the lock,
//! as a release closes a `BatchSemaphore`. The lock drops its waiters and does not wake them, and
//! after that it does not change: each request returns at once, a `try_*` fails, and an unlock does
//! nothing. So a `Drop` that takes the lock while a task unwinds does not block, and Shuttle
//! reports the panic and not a deadlock.
//!
//! # Limits
//!
//! `parking_lot`'s `try_write` also fails while `PARKED_BIT` is set on a free lock. This happens
//! after an unlock that wakes some, but not all, of the parked tasks, so it needs at least two
//! parked tasks besides the task that calls `try_write`. This model does not track parked tasks, so
//! there its `try_write` can succeed.

use shuttle_engine::annotations::{self, ObjectId};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::thread::switch;
use std::cell::RefCell;
use std::future::poll_fn;
use std::task::{Poll, Waker};
use tracing::trace;

/// The permits of the semaphore that Explorer shows for the lock, which an exclusive lock holds
/// (see the module docs). No execution has this many readers, and Explorer (JavaScript) shows the
/// number exactly.
const EXCLUSIVE: usize = 1 << 30;

/// The permits that an upgradable lock holds: a strict majority of `EXCLUSIVE`.
const UPGRADABLE: usize = EXCLUSIVE / 2 + 1;

/// The permit that a shared lock holds.
const SHARED: usize = 1;

/// The parts of `parking_lot`'s lock word that decide who can take the lock (see the module docs),
/// the tasks that wait for it to change, and what Shuttle records about the lock.
#[derive(Debug)]
struct State {
    writer: bool,
    upgradable: bool,
    readers: usize,
    /// Each waiting task, and the condition on which it can next be granted what it waits for. A
    /// task has at most one entry (see `wait_until`).
    waiters: Vec<(Waker, Ready)>,
    /// Set by a release while Shuttle stops the execution (see the module docs).
    closed: bool,
    /// The join of the clocks of all the releases of the lock (see the module docs).
    releases: VectorClock,
    /// The lock's Explorer object. It is created at first use, because `INIT` is a `const`.
    id: Option<ObjectId>,
}

impl State {
    fn id(&mut self) -> ObjectId {
        *self.id.get_or_insert_with(annotations::record_semaphore_created)
    }

    /// Make the current task's clock later than all the releases of the lock so far.
    fn join_releases(&self) {
        ExecutionState::with(|e| e.update_clock(&self.releases));
    }
}

/// The condition on which a waiting task can next be granted what it waits for.
type Ready = fn(&State) -> bool;

/// A plain read: `WRITER_BIT` is clear.
fn can_read(s: &State) -> bool {
    !s.writer
}

/// An upgradable read, or step 1 of a write: `WRITER_BIT` and `UPGRADABLE_BIT` are clear.
fn can_take_bit(s: &State) -> bool {
    !s.writer && !s.upgradable
}

/// Step 2 of a write, or an upgrade: the other readers have left.
fn readers_gone(s: &State) -> bool {
    s.readers == 0
}

/// Take one reader out of the count. The count is zero only if the caller breaks the contract of an
/// `unsafe` unlock, and then this panics in every build profile.
fn remove_reader(s: &mut State) {
    s.readers = s
        .readers
        .checked_sub(1)
        .expect("released a read lock that the lock does not hold");
}

/// A Shuttle-backed raw reader-writer lock implementing [`lock_api::RawRwLock`] and its upgrade,
/// downgrade, and fair extensions.
#[derive(Debug)]
pub struct RawRwLock {
    state: RefCell<State>,
}

// Safety: Shuttle runs one task at a time, and no borrow of `state` is held across a scheduling
// point, so two tasks never borrow it at the same time. `BatchSemaphore` relies on the same rule.
unsafe impl Send for RawRwLock {}
unsafe impl Sync for RawRwLock {}

impl RawRwLock {
    /// Wait until `take` returns `Ok`. `take` checks the state and, when it returns `Ok`, has
    /// changed it. When it returns `Err(ready)`, the task waits until `ready` is true. `take` runs
    /// when the task first calls this, and again each time the task is woken. `permits` is what
    /// Explorer shows for the request. On a closed lock, this returns at once and `take` does not
    /// run.
    fn wait_until(&self, permits: usize, mut take: impl FnMut(&mut State) -> Result<(), Ready>) {
        let mut blocked = false;
        shuttle::future::block_on(poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            if state.closed {
                return Poll::Ready(());
            }
            let id = state.id();
            match take(&mut state) {
                Ok(()) => {
                    state.join_releases();
                    if blocked {
                        annotations::record_semaphore_acquire_unblocked(id, shuttle::current::me(), permits);
                    } else {
                        annotations::record_semaphore_acquire_fast(id, permits);
                    }
                    Poll::Ready(())
                }
                Err(ready) => {
                    if !blocked {
                        annotations::record_semaphore_acquire_blocked(id, permits);
                        blocked = true;
                    }
                    // A woken task stays marked as woken until it next waits, so when its check
                    // fails it checks a second time before it sleeps. Keep one entry for the task.
                    let waker = cx.waker();
                    match state.waiters.iter_mut().find(|(other, _)| other.will_wake(waker)) {
                        Some(entry) => entry.1 = ready,
                        None => state.waiters.push((waker.clone(), ready)),
                    }
                    Poll::Pending
                }
            }
        }));
    }

    /// A `try_*` request. `take` checks the state and, when it returns `true`, has changed it. On a
    /// closed lock, this returns `false` and `take` does not run.
    fn try_take(&self, permits: usize, take: impl FnOnce(&mut State) -> bool) -> bool {
        let mut state = self.state.borrow_mut();
        if state.closed {
            return false;
        }
        let id = state.id();
        let granted = take(&mut state);
        if granted {
            state.join_releases();
        }
        annotations::record_semaphore_try_acquire(id, permits, granted);
        granted
    }

    /// An unlock or a downgrade, which gives up `permits`. Change the state, then wake each waiting
    /// task whose condition is now true, so that it checks again. No other change can make a
    /// condition true. On a closed lock, this does nothing.
    fn release(&self, permits: usize, change: impl FnOnce(&mut State)) {
        let wake = {
            let mut state = self.state.borrow_mut();
            if state.closed {
                return;
            }
            let id = state.id();
            annotations::record_semaphore_release(id, permits);
            change(&mut state);
            // While Shuttle stops an execution (for example after a panic), it must not unblock the
            // waiting tasks. Close the lock instead (see the module docs), as
            // `BatchSemaphore::release` does.
            if ExecutionState::should_stop() {
                state.waiters.clear();
                state.closed = true;
                return;
            }
            ExecutionState::with(|e| state.releases.update(e.increment_clock()));
            let (wake, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut state.waiters)
                .into_iter()
                .partition(|(_, ready)| ready(&state));
            state.waiters = wait;
            wake
        };
        wake.into_iter().for_each(|(waker, _)| waker.wake());
    }
}

unsafe impl lock_api::RawRwLock for RawRwLock {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: RawRwLock = RawRwLock {
        state: RefCell::new(State {
            writer: false,
            upgradable: false,
            readers: 0,
            waiters: Vec::new(),
            closed: false,
            releases: VectorClock::new(),
            id: None,
        }),
    };

    // Gated by `send_guard`; defined once as `crate::GuardMarker` (see `lib.rs`).
    type GuardMarker = crate::GuardMarker;

    fn lock_shared(&self) {
        trace!("acquiring parking_lot rwlock {:p} (shared)", self);
        switch();
        self.wait_until(SHARED, |s| {
            if !can_read(s) {
                return Err(can_read);
            }
            s.readers += 1;
            Ok(())
        });
        trace!("acquired parking_lot rwlock {:p} (shared)", self);
    }

    fn try_lock_shared(&self) -> bool {
        switch();
        self.try_take(SHARED, |s| {
            let granted = can_read(s);
            if granted {
                s.readers += 1;
            }
            granted
        })
    }

    unsafe fn unlock_shared(&self) {
        trace!("releasing parking_lot rwlock {:p} (shared)", self);
        switch();
        self.release(SHARED, remove_reader);
    }

    fn lock_exclusive(&self) {
        trace!("acquiring parking_lot rwlock {:p} (exclusive)", self);
        switch();
        // Step 1 takes `WRITER_BIT`. Step 2 waits for the readers to leave, and while it waits,
        // `WRITER_BIT` blocks new readers.
        let mut has_writer_bit = false;
        self.wait_until(EXCLUSIVE, |s| {
            if !has_writer_bit {
                if !can_take_bit(s) {
                    return Err(can_take_bit);
                }
                s.writer = true;
                has_writer_bit = true;
            }
            if readers_gone(s) { Ok(()) } else { Err(readers_gone) }
        });
        trace!("acquired parking_lot rwlock {:p} (exclusive)", self);
    }

    /// Unlike `parking_lot`, this can succeed while `PARKED_BIT` would be set (see the module docs).
    fn try_lock_exclusive(&self) -> bool {
        switch();
        self.try_take(EXCLUSIVE, |s| {
            let granted = can_take_bit(s) && readers_gone(s);
            if granted {
                s.writer = true;
            }
            granted
        })
    }

    unsafe fn unlock_exclusive(&self) {
        trace!("releasing parking_lot rwlock {:p} (exclusive)", self);
        switch();
        self.release(EXCLUSIVE, |s| s.writer = false);
    }

    fn is_locked(&self) -> bool {
        let s = self.state.borrow();
        s.writer || s.upgradable || s.readers > 0
    }

    fn is_locked_exclusive(&self) -> bool {
        self.state.borrow().writer
    }
}

// Safety: fair unlocking changes only which waiter `parking_lot` wakes. This model lets the
// scheduler pick any waiter, so a fair unlock is the same as a normal unlock.
unsafe impl lock_api::RawRwLockFair for RawRwLock {
    unsafe fn unlock_shared_fair(&self) {
        unsafe { lock_api::RawRwLock::unlock_shared(self) }
    }

    unsafe fn unlock_exclusive_fair(&self) {
        unsafe { lock_api::RawRwLock::unlock_exclusive(self) }
    }
}

// Safety: the caller holds `WRITER_BIT`, so no other task holds the lock. The task becomes one
// reader in the same step.
unsafe impl lock_api::RawRwLockDowngrade for RawRwLock {
    unsafe fn downgrade(&self) {
        trace!("downgrading parking_lot rwlock {:p} (exclusive -> shared)", self);
        switch();
        self.release(EXCLUSIVE - SHARED, |s| {
            s.writer = false;
            s.readers += 1;
        });
    }
}

// Safety: `UPGRADABLE_BIT` excludes writers and other upgradable readers, but not plain readers.
unsafe impl lock_api::RawRwLockUpgrade for RawRwLock {
    fn lock_upgradable(&self) {
        trace!("acquiring parking_lot rwlock {:p} (upgradable)", self);
        switch();
        self.wait_until(UPGRADABLE, |s| {
            if !can_take_bit(s) {
                return Err(can_take_bit);
            }
            s.upgradable = true;
            s.readers += 1;
            Ok(())
        });
        trace!("acquired parking_lot rwlock {:p} (upgradable)", self);
    }

    fn try_lock_upgradable(&self) -> bool {
        switch();
        self.try_take(UPGRADABLE, |s| {
            let granted = can_take_bit(s);
            if granted {
                s.upgradable = true;
                s.readers += 1;
            }
            granted
        })
    }

    unsafe fn unlock_upgradable(&self) {
        trace!("releasing parking_lot rwlock {:p} (upgradable)", self);
        switch();
        self.release(UPGRADABLE, |s| {
            s.upgradable = false;
            remove_reader(s);
        });
    }

    unsafe fn upgrade(&self) {
        trace!("upgrading parking_lot rwlock {:p} (upgradable -> exclusive)", self);
        switch();
        // Swap `UPGRADABLE_BIT` and our reader for `WRITER_BIT` in one step, as `parking_lot` does,
        // so no writer can get in during the upgrade. Then wait for the other readers to leave.
        let mut has_writer_bit = false;
        self.wait_until(EXCLUSIVE - UPGRADABLE, |s| {
            if !has_writer_bit {
                s.upgradable = false;
                remove_reader(s);
                s.writer = true;
                has_writer_bit = true;
            }
            if readers_gone(s) { Ok(()) } else { Err(readers_gone) }
        });
    }

    unsafe fn try_upgrade(&self) -> bool {
        switch();
        self.try_take(EXCLUSIVE - UPGRADABLE, |s| {
            let granted = s.readers == 1;
            if granted {
                s.upgradable = false;
                s.readers = 0;
                s.writer = true;
            }
            granted
        })
    }
}

// Safety: both conversions keep the caller in the lock, and give it a weaker mode in one step.
unsafe impl lock_api::RawRwLockUpgradeDowngrade for RawRwLock {
    unsafe fn downgrade_upgradable(&self) {
        trace!("downgrading parking_lot rwlock {:p} (upgradable -> shared)", self);
        switch();
        self.release(UPGRADABLE - SHARED, |s| s.upgradable = false);
    }

    unsafe fn downgrade_to_upgradable(&self) {
        trace!("downgrading parking_lot rwlock {:p} (exclusive -> upgradable)", self);
        switch();
        self.release(EXCLUSIVE - UPGRADABLE, |s| {
            s.writer = false;
            s.upgradable = true;
            s.readers += 1;
        });
    }
}

// Safety: as for `RawRwLockFair`.
unsafe impl lock_api::RawRwLockUpgradeFair for RawRwLock {
    unsafe fn unlock_upgradable_fair(&self) {
        unsafe { lock_api::RawRwLockUpgrade::unlock_upgradable(self) }
    }
}
