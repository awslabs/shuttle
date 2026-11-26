//! The Shuttle-backed raw mutex that underpins [`crate::Mutex`].
//!
//! This provides [`RawMutex`], an implementation of [`lock_api::RawMutex`] (and the fair and timed
//! variants) routed through Shuttle's scheduler. The user-facing `Mutex`/`MutexGuard`/`ArcMutexGuard`
//! are the generic `lock_api` types specialised to this raw lock (see [`crate::Mutex`]), exactly how
//! the real `parking_lot` crate layers its `Mutex` on top of `lock_api`.

use crate::per_execution::Handle;
use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use shuttle_engine::runtime::per_execution::ValueLock;
use shuttle_engine::runtime::thread::switch;
use shuttle_engine::sync_types::{ResourceSignature, ResourceType};
use std::cell::Cell;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};
use tracing::trace;

/// A Shuttle-backed raw mutex implementing [`lock_api::RawMutex`].
///
/// The lock is modelled as a [`BatchSemaphore`] with a single permit: acquiring the lock takes the
/// permit, releasing it returns the permit. All blocking happens through Shuttle's scheduler, so
/// every lock/unlock is a scheduling point that Shuttle can explore.
///
/// The semaphore belongs to the running execution, not to the lock (see `per_execution`), so a
/// `static` mutex starts every execution unlocked, with no waiters, whatever the execution before
/// left behind. The value the mutex protects belongs to the lock, though, so an execution that
/// locks the mutex while a guard of another execution holds the value panics rather than hand out
/// a second reference to it: a guard of a test that runs at the same time on another thread, or one
/// that an ended execution left behind (one that a failed execution leaked, or a forgotten one)
/// (see [`ValueLock`]).
#[derive(Debug)]
pub struct RawMutex {
    handle: Handle,
    /// Keeps the value of the lock to one execution at a time, which the state, being per
    /// execution, can't (see [`ValueLock`]).
    value: ValueLock,
}

/// The name of the lock in the message of [`ValueLock::acquire`].
const KIND: &str = "parking_lot Mutex";

/// A mutex's state in one execution.
struct State {
    /// One permit, held by whoever holds the lock.
    semaphore: BatchSemaphore,
    /// Unlocks to ignore, because the lock wasn't held when they were called. Only a task that
    /// execution teardown unwinds out of `Condvar::wait` leaves one (see `expect_unheld_unlock`).
    unheld_unlocks: Cell<usize>,
}

impl RawMutex {
    fn state(&self) -> Rc<State> {
        self.handle.state(|| State {
            semaphore: BatchSemaphore::const_new_with_signature(
                1,
                Fairness::StrictlyFair,
                ResourceSignature::new_const(ResourceType::Mutex),
            ),
            unheld_unlocks: Cell::new(0),
        })
    }

    /// The id that tells this mutex apart from others, for `Condvar`.
    pub(crate) fn id(&self) -> usize {
        self.handle.id()
    }

    /// Unlocks without a scheduling point, for `Condvar::wait`, which has taken one already: it
    /// unlocks and starts to wait in one step.
    ///
    /// The wait gives the value up with the lock, though the caller keeps its guard: a waiting task
    /// can't use the value, and the `lock` that ends the wait takes it back.
    ///
    /// # Safety
    ///
    /// The mutex must be held, as for [`lock_api::RawMutex::unlock`].
    pub(crate) unsafe fn unlock_without_scheduling_point(&self) {
        trace!("releasing parking_lot mutex {:p} to wait on a condvar", self);
        self.value.release();
        self.state().semaphore.release_no_scheduling_point(1);
    }

    /// Makes the next unlock a no-op. `Condvar::wait` calls this when execution teardown unwinds
    /// its task while it waits: the caller's guard will unlock a mutex that `wait` released and
    /// never took back.
    pub(crate) fn expect_unheld_unlock(&self) {
        let state = self.state();
        state.unheld_unlocks.set(state.unheld_unlocks.get() + 1);
    }
}

// Safety: `RawMutex` guarantees exclusivity because the underlying semaphore has exactly one permit,
// so at most one context can hold the lock at a time.
unsafe impl lock_api::RawMutex for RawMutex {
    // A "non-constant" const item is the legacy `lock_api` mechanism for supplying an initial value
    // to a `const`-constructed lock. Because a `const` is a value template (not a shared `static`),
    // every `Mutex::new` materialises a fresh handle, which takes an id at its first use.
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: RawMutex = RawMutex {
        handle: Handle::new(),
        value: ValueLock::new(),
    };

    // Gated by `send_guard`; defined once as `crate::GuardMarker` (see `lib.rs`).
    type GuardMarker = crate::GuardMarker;

    fn lock(&self) {
        let state = self.state();
        trace!("acquiring parking_lot mutex {:p}", self);
        state.semaphore.acquire_blocking(1).unwrap_or_else(|_| {
            // The semaphore is never explicitly closed and we own it exclusively, so a closed
            // semaphore here can only be observed while unwinding from a panic.
            if !thread::panicking() {
                unreachable!()
            }
        });
        self.value.acquire(KIND);
        trace!("acquired parking_lot mutex {:p}", self);
    }

    fn try_lock(&self) -> bool {
        let acquired = self.state().semaphore.try_acquire(1).is_ok();
        if acquired {
            self.value.acquire(KIND);
        }
        acquired
    }

    unsafe fn unlock(&self) {
        let state = self.state();
        let unheld = state.unheld_unlocks.get();
        if unheld > 0 {
            // `Condvar::wait` gave up the lock and the value; this unlock is the caller's guard's.
            state.unheld_unlocks.set(unheld - 1);
            return;
        }
        trace!("releasing parking_lot mutex {:p}", self);
        self.value.release();
        state.semaphore.release(1);
    }

    // `lock_api`'s default takes and releases the lock, so a task asking whether the lock is held
    // would make another task's `try_lock` fail, which can't happen with `parking_lot`, where this is
    // a load. Here it is a load too, after a scheduling point, as an atomic load is in Shuttle.
    fn is_locked(&self) -> bool {
        switch();
        self.state().semaphore.available_permits() == 0
    }
}

// Safety: Shuttle's semaphore is strictly fair, so a plain `release` already hands the permit to the
// next waiter in FIFO order. Fair unlocking is therefore identical to a normal unlock.
unsafe impl lock_api::RawMutexFair for RawMutex {
    unsafe fn unlock_fair(&self) {
        trace!("fair-releasing parking_lot mutex {:p}", self);
        // Safety: the caller holds the lock, as `unlock_fair` requires.
        unsafe { lock_api::RawMutex::unlock(self) };
    }
}

// Safety: a timed lock is a lock that never times out (see below), so it is as exclusive as `lock`.
//
// Shuttle has no clock that a timeout could be measured against, so a timed lock waits until it
// gets the lock and returns `true`, as Shuttle's `Condvar::wait_timeout` never times out. A task
// that would time out in a real run blocks instead; if nothing ever releases the lock, Shuttle
// reports a deadlock.
unsafe impl lock_api::RawMutexTimed for RawMutex {
    type Duration = Duration;
    type Instant = Instant;

    fn try_lock_for(&self, _timeout: Duration) -> bool {
        lock_api::RawMutex::lock(self);
        true
    }

    fn try_lock_until(&self, _timeout: Instant) -> bool {
        lock_api::RawMutex::lock(self);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::RawMutex;
    use lock_api::RawMutex as _;
    use shuttle::check_dfs;

    /// `Condvar::wait` releases the mutex itself, and the caller's guard unlocks it again if
    /// teardown unwinds the waiting task. The second unlock must not return a second permit, which
    /// would let two tasks hold the lock at once.
    #[test]
    fn an_unheld_unlock_is_ignored() {
        check_dfs(
            || {
                let mutex = RawMutex::INIT;
                mutex.lock();
                assert!(mutex.is_locked());
                // The wait's release.
                // Safety: this task holds the lock.
                unsafe { mutex.unlock_without_scheduling_point() };
                assert!(!mutex.is_locked());
                // The guard's unlock, as the task is unwound.
                mutex.expect_unheld_unlock();
                // Safety: the unlock is the one the call above announced.
                unsafe { mutex.unlock() };
                assert!(!mutex.is_locked());
                // The lock is still exclusive: there is one permit, not two.
                assert!(mutex.try_lock());
                assert!(!mutex.try_lock());
            },
            None,
        );
    }
}
