//! The user-facing `ReentrantMutex` types, mirroring [`parking_lot`]'s `remutex` module.
//!
//! These are the generic [`lock_api`] `ReentrantMutex` types specialised to Shuttle's
//! [`RawMutex`](crate::RawMutex) and to [`RawThreadId`], which tells Shuttle's tasks apart. The
//! `Arc`-based [`ArcReentrantMutexGuard`](lock_api::ArcReentrantMutexGuard) is re-exported from the
//! crate root (see `lib.rs`), matching `parking_lot`.
//!
//! A task that already holds the lock takes it again without touching the raw mutex, so only the
//! outermost `lock` and the last unlock are scheduling points.
//!
//! [`parking_lot`]: <https://crates.io/crates/parking_lot>

use crate::per_execution;
use crate::raw_mutex::RawMutex;
use core::num::NonZeroUsize;
use lock_api::GetThreadId;

/// Identifies the running Shuttle task, for [`ReentrantMutex`]. Mirrors `parking_lot::RawThreadId`,
/// which identifies the running thread.
///
/// Shuttle runs every task of an execution on one thread, so `parking_lot`'s thread-local address
/// would make every task the owner of a lock that any of them holds. The ids this hands out are
/// unique to a task of one execution, so a lock that a task of an earlier execution left locked
/// never counts as held by a task of a later one.
pub struct RawThreadId;

// Safety: `per_execution::thread_id` never gives two tasks the same id, in any execution.
unsafe impl GetThreadId for RawThreadId {
    const INIT: RawThreadId = RawThreadId;

    fn nonzero_thread_id(&self) -> NonZeroUsize {
        per_execution::thread_id()
    }
}

/// A mutex which can be recursively locked by a single task, backed by Shuttle. Mirrors
/// `parking_lot::ReentrantMutex`.
pub type ReentrantMutex<T> = lock_api::ReentrantMutex<RawMutex, RawThreadId, T>;

/// Creates a new reentrant mutex in an unlocked state ready for use.
///
/// This allows creating a reentrant mutex in a constant context on stable Rust. Mirrors
/// `parking_lot::const_reentrant_mutex`.
pub const fn const_reentrant_mutex<T>(val: T) -> ReentrantMutex<T> {
    ReentrantMutex::const_new(
        <RawMutex as lock_api::RawMutex>::INIT,
        <RawThreadId as lock_api::GetThreadId>::INIT,
        val,
    )
}

/// An RAII scoped lock guard for a [`ReentrantMutex`]. Mirrors `parking_lot::ReentrantMutexGuard`.
pub type ReentrantMutexGuard<'a, T> = lock_api::ReentrantMutexGuard<'a, RawMutex, RawThreadId, T>;

/// An RAII mutex guard returned by `ReentrantMutexGuard::map`. Mirrors
/// `parking_lot::MappedReentrantMutexGuard`.
pub type MappedReentrantMutexGuard<'a, T> = lock_api::MappedReentrantMutexGuard<'a, RawMutex, RawThreadId, T>;
