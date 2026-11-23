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

// SAFETY:
// In order for this impl to be safe, we need to ensure:
// * The mutex is exclusive: `lock` and `try_lock` can't succeed while the mutex is locked.
// * With `send_guard`, which makes `GuardMarker` `GuardSend`, `unlock` works from another thread
//   than the one that locked.
//
// The mutex is a `BatchSemaphore` with one permit, which `INIT` makes. `lock` and `try_lock` take
// the permit, and only `unlock` and `unlock_fair` give it back. lock_api calls `unlock` and
// `unlock_fair` only for a mutex that is held, as their `# Safety` sections ask, so the semaphore
// never has more than one permit. While the permit is out, `try_acquire` fails and
// `acquire_blocking` waits.
// The semaphore doesn't care which task gives a permit back, and Shuttle runs all the tasks of an
// execution on one OS thread. So a guard can be dropped in any Shuttle thread.
// The second property holds only for the threads that Shuttle runs. With `send_guard`, safe code
// can move a guard into a thread that Shuttle doesn't run, for example one from
// `std::thread::scope`, and drop it there. The release then uses the semaphore's `RefCell` from
// that OS thread, where no execution is set. That is not sound (see the `Sync` impl of
// `BatchSemaphore`).
// The first property does not hold in one case. When the acquire fails because the semaphore is
// closed, and the thread is panicking, `lock` returns without the permit. Shuttle closes a semaphore
// when a release runs where `ExecutionState::should_stop` is true: after the execution has stopped
// or failed, while Shuttle's own state is borrowed, or outside an execution, for example when a
// guard outlives its test (see `BatchSemaphore::release_no_scheduling_point`). From then on, each
// `lock` while panicking succeeds. So two guards can exist at once, for example when a panicking
// thread's destructor locks the mutex while that thread already holds a guard of it.
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

// SAFETY:
// In order for this impl to be safe, we need to ensure:
// * `unlock_fair` keeps the mutex exclusive, as `unlock` does.
//
// `unlock_fair` gives back the one permit, as `unlock` does. The semaphore is strictly fair, so a
// plain release already hands the permit to the first waiter in FIFO order. The default `bump`
// calls `unlock_fair` and then `lock`, so it keeps the mutex exclusive too.
unsafe impl lock_api::RawMutexFair for RawMutex {
    unsafe fn unlock_fair(&self) {
        trace!("fair-releasing parking_lot mutex {:p}", self);
        self.semaphore.release(1);
    }
}
