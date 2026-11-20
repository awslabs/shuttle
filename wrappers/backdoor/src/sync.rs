//! The `std::sync` locks, `Condvar`, `Barrier` and once-initialized cells, invisible to Shuttle.
//!
//! Each type is an alias of the `std` type, so it is the `std` type itself.

pub mod atomic;
pub mod mpsc;

/// [`std::sync::Mutex`], invisible to Shuttle.
pub type Mutex<T> = std::sync::Mutex<T>;

/// [`std::sync::MutexGuard`], invisible to Shuttle.
pub type MutexGuard<'a, T> = std::sync::MutexGuard<'a, T>;

/// [`std::sync::RwLock`], invisible to Shuttle.
pub type RwLock<T> = std::sync::RwLock<T>;

/// [`std::sync::RwLockReadGuard`], invisible to Shuttle.
pub type RwLockReadGuard<'a, T> = std::sync::RwLockReadGuard<'a, T>;

/// [`std::sync::RwLockWriteGuard`], invisible to Shuttle.
pub type RwLockWriteGuard<'a, T> = std::sync::RwLockWriteGuard<'a, T>;

/// [`std::sync::Condvar`], invisible to Shuttle.
pub type Condvar = std::sync::Condvar;

/// [`std::sync::Barrier`], invisible to Shuttle.
pub type Barrier = std::sync::Barrier;

/// [`std::sync::Once`], invisible to Shuttle.
pub type Once = std::sync::Once;

/// [`std::sync::OnceLock`], invisible to Shuttle.
pub type OnceLock<T> = std::sync::OnceLock<T>;

/// [`std::sync::LazyLock`], invisible to Shuttle.
pub type LazyLock<T, F = fn() -> T> = std::sync::LazyLock<T, F>;

// The results and errors of the types above. The lint gate doesn't deny them, so plain
// re-exports pass it, and they let a `use` list move over from `std::sync` unchanged.
pub use std::sync::{
    BarrierWaitResult, LockResult, OnceState, PoisonError, TryLockError, TryLockResult, WaitTimeoutResult,
};
