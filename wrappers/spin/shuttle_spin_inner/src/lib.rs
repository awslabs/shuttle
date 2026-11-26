//! This crate contains Shuttle's internal implementations of the [`spin`] crate, version 0.10. Do
//! not depend on this crate directly. Use the `shuttle-spin` crate, which conditionally exposes
//! these implementations with the `shuttle` feature or the original crate without it.
//!
//! [`spin`]: <https://crates.io/crates/spin/0.10.1>
//!
//! # Under Shuttle
//!
//! Each of spin's primitives is an atomic word and a loop that retries an operation on it. The
//! primitives here keep the same word, with spin's encoding, and run spin's operations on it, so
//! they have spin's API and its rules: an upgradeable guard keeps new readers out, a ticket lock
//! serves its waiters in order, a `Once` that panicked stays poisoned. What differs is how a task
//! waits:
//!
//! - **A waiting task blocks instead of spinning.** Shuttle runs every task of a test on one
//!   thread, so a task that spins for a lock that another task holds never lets that task run.
//!   Here a task whose operation fails blocks until another task changes the word, and then
//!   retries. That reaches the same states as spinning: until the word changes, a retry fails
//!   again. If every task is blocked, Shuttle reports a deadlock.
//! - **Each operation on the word is a scheduling point**, as each operation of one of Shuttle's
//!   atomics is: taking a lock, trying to, releasing a guard, an upgrade or downgrade, the
//!   `force_*` methods, and the reads `is_locked`, `reader_count` and `writer_count`. The
//!   exceptions follow Shuttle's `OnceLock`: reading a complete [`Once`] takes no scheduling point,
//!   and neither do `get_mut`, `into_inner` or formatting with `Debug`.
//! - **State resets per execution.** The word lives in the storage of the running execution, so a
//!   `static` lock is unlocked at the start of every execution, with no waiters, and a `static`
//!   [`Once`] or [`Lazy`] is uninitialized again, its initializer running once in each execution.
//!   A lock's value lives in the lock, so that `Mutex<[u8]>` and `Mutex<dyn Trait>` work as they
//!   do in spin, and so it is reset only when that can be done soundly: when the value needs no
//!   dropping (a number, a `bool`, `Option<&'static str>`), it is put back to what it was when the
//!   first execution used the lock. A value that owns memory (a `Vec`, a `String`, a `Box`)
//!   carries over from one execution into the next, as it would in a plain `static`.
//! - **Relax strategies are not called.** The locks never spin, so they never call one. Calling
//!   [`RelaxStrategy::relax`] yourself takes a scheduling point.
//!
//! [`Once`] and [`Lazy`] keep their values in the execution's storage, so storing one requires
//! `T: 'static`, and [`Lazy`]'s initializer must be `Fn` rather than `FnOnce`, as it runs in every
//! execution. `try_lock_weak`, `try_write_weak` and `try_upgrade_weak` never fail spuriously. A
//! `FairMutex`'s `lock` never starves on its own (see its documentation).
//! Leaking a guard locks the lock in every later execution too, as it would with spin.
//!
//! Two executions can't hold one lock at once, even both to read it, and even if its value is
//! copied into each: a lock can guard data outside of itself, as the `Mutex<()>` under a `lock_api`
//! lock does. An execution that takes a lock while a guard of another execution holds it panics,
//! rather than hand out a second reference, and says whose guard it is, as Shuttle's other locks do
//! (see `shuttle-engine`'s `ValueLock`): one of a test that runs at the same time on another thread
//! and uses the same `static`, which has to take turns with it, or one that an ended execution left
//! behind, forgotten with `mem::forget`, which holds the lock for good. A guard leaked with its
//! `leak` method keeps the lock's word locked in later executions instead, as spin does, until a
//! `force_*` method releases it; a later reader beside a leaked read guard reports it as left
//! behind.

#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

// Every primitive needs some of it; builds with only some of the primitives leave the rest unused.
#[cfg(any(feature = "mutex", feature = "rwlock", feature = "once"))]
#[cfg_attr(not(all(feature = "mutex", feature = "rwlock", feature = "once")), allow(dead_code))]
mod state;
#[cfg(any(feature = "mutex", feature = "rwlock"))]
mod value;

#[cfg(feature = "barrier")]
#[cfg_attr(docsrs, doc(cfg(feature = "barrier")))]
pub mod barrier;
#[cfg(feature = "lazy")]
#[cfg_attr(docsrs, doc(cfg(feature = "lazy")))]
pub mod lazy;
#[cfg(feature = "mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "mutex")))]
pub mod mutex;
#[cfg(feature = "once")]
#[cfg_attr(docsrs, doc(cfg(feature = "once")))]
pub mod once;
pub mod relax;
#[cfg(feature = "rwlock")]
#[cfg_attr(docsrs, doc(cfg(feature = "rwlock")))]
pub mod rwlock;

#[cfg(feature = "mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "mutex")))]
pub use mutex::MutexGuard;
#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
pub use relax::Yield;
pub use relax::{RelaxStrategy, Spin};
#[cfg(feature = "rwlock")]
#[cfg_attr(docsrs, doc(cfg(feature = "rwlock")))]
pub use rwlock::RwLockReadGuard;

// Avoid confusing inference errors by aliasing away the relax strategy parameter. Users that need
// to use a different relax strategy can do so by accessing the types through their
// fully-qualified path. These are spin's aliases.

/// A primitive that synchronizes the execution of multiple threads. See [`barrier::Barrier`] for
/// documentation.
///
/// A note for advanced users: this alias exists to avoid subtle type inference errors due to the
/// default relax strategy type parameter. If you need a non-default relax strategy, use the
/// fully-qualified path.
#[cfg(feature = "barrier")]
#[cfg_attr(docsrs, doc(cfg(feature = "barrier")))]
pub type Barrier = crate::barrier::Barrier;

/// A value which is initialized on the first access. See [`lazy::Lazy`] for documentation.
///
/// A note for advanced users: this alias exists to avoid subtle type inference errors due to the
/// default relax strategy type parameter. If you need a non-default relax strategy, use the
/// fully-qualified path.
#[cfg(feature = "lazy")]
#[cfg_attr(docsrs, doc(cfg(feature = "lazy")))]
pub type Lazy<T, F = fn() -> T> = crate::lazy::Lazy<T, F>;

/// A primitive that synchronizes the execution of multiple threads. See [`mutex::Mutex`] for
/// documentation.
///
/// A note for advanced users: this alias exists to avoid subtle type inference errors due to the
/// default relax strategy type parameter. If you need a non-default relax strategy, use the
/// fully-qualified path.
#[cfg(feature = "mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "mutex")))]
pub type Mutex<T> = crate::mutex::Mutex<T>;

/// A primitive that provides lazy one-time initialization. See [`once::Once`] for documentation.
///
/// A note for advanced users: this alias exists to avoid subtle type inference errors due to the
/// default relax strategy type parameter. If you need a non-default relax strategy, use the
/// fully-qualified path.
#[cfg(feature = "once")]
#[cfg_attr(docsrs, doc(cfg(feature = "once")))]
pub type Once<T = ()> = crate::once::Once<T>;

/// A lock that provides data access to either one writer or many readers. See [`rwlock::RwLock`]
/// for documentation.
///
/// A note for advanced users: this alias exists to avoid subtle type inference errors due to the
/// default relax strategy type parameter. If you need a non-default relax strategy, use the
/// fully-qualified path.
#[cfg(feature = "rwlock")]
#[cfg_attr(docsrs, doc(cfg(feature = "rwlock")))]
pub type RwLock<T> = crate::rwlock::RwLock<T>;

/// A guard that provides immutable data access but can be upgraded to [`RwLockWriteGuard`]. See
/// [`rwlock::RwLockUpgradableGuard`] for documentation.
///
/// A note for advanced users: this alias exists to avoid subtle type inference errors due to the
/// default relax strategy type parameter. If you need a non-default relax strategy, use the
/// fully-qualified path.
#[cfg(feature = "rwlock")]
#[cfg_attr(docsrs, doc(cfg(feature = "rwlock")))]
pub type RwLockUpgradableGuard<'a, T> = crate::rwlock::RwLockUpgradableGuard<'a, T>;

/// A guard that provides mutable data access. See [`rwlock::RwLockWriteGuard`] for documentation.
///
/// A note for advanced users: this alias exists to avoid subtle type inference errors due to the
/// default relax strategy type parameter. If you need a non-default relax strategy, use the
/// fully-qualified path.
#[cfg(feature = "rwlock")]
#[cfg_attr(docsrs, doc(cfg(feature = "rwlock")))]
pub type RwLockWriteGuard<'a, T> = crate::rwlock::RwLockWriteGuard<'a, T>;

/// Spin synchronisation primitives, but compatible with [`lock_api`](https://crates.io/crates/lock_api).
#[cfg(feature = "lock_api")]
#[cfg_attr(docsrs, doc(cfg(feature = "lock_api")))]
pub mod lock_api {
    /// A lock that provides mutually exclusive data access (compatible with
    /// [`lock_api`](https://crates.io/crates/lock_api)).
    #[cfg(feature = "mutex")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mutex")))]
    pub type Mutex<T> = lock_api_crate::Mutex<crate::Mutex<()>, T>;

    /// A guard that provides mutable data access (compatible with
    /// [`lock_api`](https://crates.io/crates/lock_api)).
    #[cfg(feature = "mutex")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mutex")))]
    pub type MutexGuard<'a, T> = lock_api_crate::MutexGuard<'a, crate::Mutex<()>, T>;

    /// A lock that provides data access to either one writer or many readers (compatible with
    /// [`lock_api`](https://crates.io/crates/lock_api)).
    #[cfg(feature = "rwlock")]
    #[cfg_attr(docsrs, doc(cfg(feature = "rwlock")))]
    pub type RwLock<T> = lock_api_crate::RwLock<crate::RwLock<()>, T>;

    /// A guard that provides immutable data access (compatible with
    /// [`lock_api`](https://crates.io/crates/lock_api)).
    #[cfg(feature = "rwlock")]
    #[cfg_attr(docsrs, doc(cfg(feature = "rwlock")))]
    pub type RwLockReadGuard<'a, T> = lock_api_crate::RwLockReadGuard<'a, crate::RwLock<()>, T>;

    /// A guard that provides mutable data access (compatible with
    /// [`lock_api`](https://crates.io/crates/lock_api)).
    #[cfg(feature = "rwlock")]
    #[cfg_attr(docsrs, doc(cfg(feature = "rwlock")))]
    pub type RwLockWriteGuard<'a, T> = lock_api_crate::RwLockWriteGuard<'a, crate::RwLock<()>, T>;

    /// A guard that provides immutable data access but can be upgraded to [`RwLockWriteGuard`]
    /// (compatible with [`lock_api`](https://crates.io/crates/lock_api)).
    #[cfg(feature = "rwlock")]
    #[cfg_attr(docsrs, doc(cfg(feature = "rwlock")))]
    pub type RwLockUpgradableReadGuard<'a, T> = lock_api_crate::RwLockUpgradableReadGuard<'a, crate::RwLock<()>, T>;
}
