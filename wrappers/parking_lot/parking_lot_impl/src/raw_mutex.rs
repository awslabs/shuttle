//! The Shuttle-backed raw mutex that underpins [`crate::Mutex`].
//!
//! This provides [`RawMutex`], an implementation of [`lock_api::RawMutex`] (and the fair and timed
//! variants) routed through Shuttle's scheduler. The user-facing `Mutex`/`MutexGuard`/`ArcMutexGuard`
//! are the generic `lock_api` types specialised to this raw lock (see [`crate::Mutex`]), exactly how
//! the real `parking_lot` crate layers its `Mutex` on top of `lock_api`.

use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use shuttle_engine::runtime::per_execution::ValueLock;
use shuttle_engine::runtime::thread::switch;
use std::thread;
use std::time::{Duration, Instant};
use tracing::trace;

/// A Shuttle-backed raw mutex implementing [`lock_api::RawMutex`].
///
/// The lock is modelled as a [`BatchSemaphore`] with a single permit: acquiring the lock takes the
/// permit, releasing it returns the permit. All blocking happens through Shuttle's scheduler, so
/// every lock/unlock is a scheduling point that Shuttle can explore.
///
/// The semaphore's state belongs to the running execution, not to the lock (see
/// [`BatchSemaphore`]), so a `static` mutex starts every execution unlocked, with no waiters,
/// whatever the execution before left behind. The value the mutex protects belongs to the lock,
/// though, so an execution that locks the mutex while a guard of another execution holds the value
/// panics rather than hand out a second reference to it: a guard of a test that runs at the same
/// time on another thread, or one that an ended execution left behind (one that a failed execution
/// leaked, or a forgotten one) (see [`ValueLock`]).
#[derive(Debug)]
pub struct RawMutex {
    semaphore: BatchSemaphore,
    /// Keeps the value of the lock to one execution at a time, which the semaphore, being per
    /// execution, can't (see [`ValueLock`]).
    value: ValueLock,
}

/// The name of the lock in the message of [`ValueLock::acquire`].
const KIND: &str = "parking_lot Mutex";

// Safety: `RawMutex` guarantees exclusivity because the underlying semaphore has exactly one permit,
// so at most one context can hold the lock at a time.
unsafe impl lock_api::RawMutex for RawMutex {
    // A "non-constant" const item is the legacy `lock_api` mechanism for supplying an initial value
    // to a `const`-constructed lock. `BatchSemaphore::const_new` lets us honour it. Because a `const`
    // is a value template (not a shared `static`), every `Mutex::new` materialises a fresh
    // semaphore; the semaphore registers with the current Shuttle execution lazily on first use.
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: RawMutex = RawMutex {
        semaphore: BatchSemaphore::const_new(1, Fairness::StrictlyFair),
        value: ValueLock::new(),
    };

    // Gated by `send_guard`; defined once as `crate::GuardMarker` (see `lib.rs`).
    type GuardMarker = crate::GuardMarker;

    fn lock(&self) {
        trace!("acquiring parking_lot mutex {:p}", self);
        self.semaphore.acquire_blocking(1).unwrap_or_else(|_| {
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
        let acquired = self.semaphore.try_acquire(1).is_ok();
        if acquired {
            self.value.acquire(KIND);
        }
        acquired
    }

    unsafe fn unlock(&self) {
        trace!("releasing parking_lot mutex {:p}", self);
        self.value.release();
        self.semaphore.release(1);
    }

    // `lock_api`'s default takes and releases the lock, so a task asking whether the lock is held
    // would make another task's `try_lock` fail, which can't happen with `parking_lot`, where this is
    // a load. Here it is a load too, after a scheduling point, as an atomic load is in Shuttle.
    fn is_locked(&self) -> bool {
        switch();
        self.semaphore.available_permits() == 0
    }
}

// Safety: Shuttle's semaphore is strictly fair, so a plain `release` already hands the permit to the
// next waiter in FIFO order. Fair unlocking is therefore identical to a normal unlock.
unsafe impl lock_api::RawMutexFair for RawMutex {
    unsafe fn unlock_fair(&self) {
        trace!("fair-releasing parking_lot mutex {:p}", self);
        self.value.release();
        self.semaphore.release(1);
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
