//! The Shuttle-backed raw mutex that underpins [`crate::Mutex`].
//!
//! This provides [`RawMutex`], an implementation of [`lock_api::RawMutex`] (and the fair variant)
//! routed through Shuttle's scheduler. The user-facing `Mutex`/`MutexGuard`/`ArcMutexGuard` are the
//! generic `lock_api` types specialised to this raw lock (see [`crate::Mutex`]), exactly how the
//! real `parking_lot` crate layers its `Mutex` on top of `lock_api`.

use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use std::thread;
use tracing::trace;

/// A Shuttle-backed raw mutex implementing [`lock_api::RawMutex`].
///
/// The lock is modelled as a [`BatchSemaphore`] with a single permit: acquiring the lock takes the
/// permit, releasing it returns the permit. All blocking happens through Shuttle's scheduler, so
/// every lock/unlock is a scheduling point that Shuttle can explore.
#[derive(Debug)]
pub struct RawMutex {
    semaphore: BatchSemaphore,
}

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
        trace!("acquired parking_lot mutex {:p}", self);
    }

    fn try_lock(&self) -> bool {
        self.semaphore.try_acquire(1).is_ok()
    }

    unsafe fn unlock(&self) {
        trace!("releasing parking_lot mutex {:p}", self);
        self.semaphore.release(1);
    }
}

// Safety: Shuttle's semaphore is strictly fair, so a plain `release` already hands the permit to the
// next waiter in FIFO order. Fair unlocking is therefore identical to a normal unlock.
unsafe impl lock_api::RawMutexFair for RawMutex {
    unsafe fn unlock_fair(&self) {
        trace!("fair-releasing parking_lot mutex {:p}", self);
        self.semaphore.release(1);
    }
}
