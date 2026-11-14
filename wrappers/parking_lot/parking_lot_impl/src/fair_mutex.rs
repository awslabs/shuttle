//! The user-facing `FairMutex` types, mirroring [`parking_lot`]'s `fair_mutex` module.
//!
//! These are the generic [`lock_api`] `Mutex` types specialised to Shuttle's
//! [`RawFairMutex`](crate::RawFairMutex), which hands the lock to the longest waiter on every
//! unlock, as `parking_lot`'s does.
//!
//! [`parking_lot`]: <https://crates.io/crates/parking_lot>

use crate::raw_fair_mutex::RawFairMutex;

/// A mutual exclusion primitive that is always fair, backed by Shuttle. Mirrors
/// `parking_lot::FairMutex`.
pub type FairMutex<T> = lock_api::Mutex<RawFairMutex, T>;

/// Creates a new fair mutex in an unlocked state ready for use.
///
/// This allows creating a fair mutex in a constant context on stable Rust. Mirrors
/// `parking_lot::const_fair_mutex`.
pub const fn const_fair_mutex<T>(val: T) -> FairMutex<T> {
    FairMutex::const_new(<RawFairMutex as lock_api::RawMutex>::INIT, val)
}

/// An RAII scoped lock guard for a [`FairMutex`]. Mirrors `parking_lot::FairMutexGuard`.
pub type FairMutexGuard<'a, T> = lock_api::MutexGuard<'a, RawFairMutex, T>;

/// An RAII mutex guard returned by `FairMutexGuard::map`. Mirrors `parking_lot::MappedFairMutexGuard`.
pub type MappedFairMutexGuard<'a, T> = lock_api::MappedMutexGuard<'a, RawFairMutex, T>;
