//! This crate contains Shuttle's internal implementations of the `parking_lot` crate.
//! Do not depend on this crate directly. Use the `shuttle-parking_lot` crate, which conditionally
//! exposes these implementations with the `shuttle` feature or the original crate without it.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//!
//! [`parking_lot`]: <https://crates.io/crates/parking_lot>
//!
//! # Architecture
//!
//! These implementations are built on top of the [`lock_api`] crate, exactly like the real
//! `parking_lot` crate, and the module layout mirrors `parking_lot`'s:
//!
//! * `raw_mutex` / `raw_rwlock` contain the Shuttle-backed *raw* locks (`RawMutex` / `RawRwLock`,
//!   built on Shuttle's `BatchSemaphore`) that route their blocking through Shuttle's scheduler by
//!   implementing the relevant `lock_api` raw traits.
//! * `mutex` / `rwlock` contain the user-facing `Mutex` / `RwLock` types, which are the generic
//!   `lock_api` containers and guards specialised to those raw locks.
//! * `raw_fair_mutex` / `fair_mutex` and `remutex` do the same for `FairMutex` and `ReentrantMutex`,
//!   as `parking_lot` does.
//! * `condvar` and `once` are Shuttle models of `parking_lot`'s `Condvar` and `Once`, which
//!   `parking_lot` doesn't build on `lock_api` either.
//!
//! This layering means the entire `parking_lot` guard surface — `Arc` guards, mapped guards,
//! upgradable guards — is available under Shuttle without reimplementing it by hand.
//!
//! # State that outlives an execution
//!
//! Every primitive here can be `const`-constructed and live in a `static`. Its Shuttle state (a
//! lock's semaphore, a condvar's waiters, a `Once`'s progress) belongs to the running execution
//! (see `per_execution`), so every execution starts with the primitive unlocked, unwaited and
//! uncompleted, whatever the execution before it did, and executions running at the same time on
//! other threads don't share it. What a lock protects is not part of that state: `lock_api` keeps
//! the data inside the lock, so the data of a `static` lock carries over from one execution to the
//! next. For the same reason two executions can't both hold a lock's data, even both to read it:
//! one whose own state grants it the lock while a guard of another execution holds the data panics
//! rather than hand out a second reference to the data, and says whose guard it is, one of a test
//! that runs at the same time on another thread or one that an ended execution left behind (one
//! that a failed execution leaked, or a forgotten one), which holds the data for good (see
//! `shuttle-engine`'s `ValueLock`). Tests that share a `static` lock have to take turns. A lock that
//! a test needs to start afresh in every execution belongs in a `shuttle::lazy_static!`, which
//! creates it anew in each one.
//!
//! # Timeouts
//!
//! Shuttle has no clock to measure a timeout against, so the timed operations never time out: the
//! `*_for` and `*_until` lock methods wait until they get the lock and return `true`, and a timed
//! `Condvar` wait waits until it is notified. A task that would time out in a real run waits
//! instead, and if nothing ever releases or notifies it, Shuttle reports a deadlock.

mod condvar;
mod fair_mutex;
mod mutex;
mod once;
mod per_execution;
mod raw_fair_mutex;
mod raw_mutex;
mod raw_rwlock;
mod remutex;
mod rwlock;

// The marker that determines whether lock guards are `Send`, defined once here and referenced as
// `crate::GuardMarker` by both raw lock impls (mirroring `parking_lot`'s own `lib.rs`). It gates
// `Send` *in addition to* the data type: `lock_api`'s guards carry a `PhantomData<(&mut T,
// GuardMarker)>`, so a guard is `Send` only when both the data permits it and this marker is `Send`.
// We follow `parking_lot`'s `send_guard` feature exactly — guards are `!Send` by default and become
// `Send` (when the data allows) with `send_guard` — so the Shuttle build's `Send` contract is
// identical to production `parking_lot`. This is a private implementation detail, not public API.
#[cfg(feature = "send_guard")]
type GuardMarker = ::lock_api::GuardSend;
#[cfg(not(feature = "send_guard"))]
type GuardMarker = ::lock_api::GuardNoSend;

// Re-export the public surface explicitly (rather than globbing the modules) so the crate's public
// API is a deliberate, reviewable list that mirrors `parking_lot`'s own `lib.rs`. This matters
// because `shuttle-parking_lot` re-exports this crate wholesale, so anything public here becomes
// part of the client-facing `parking_lot` namespace.
pub use self::condvar::{Condvar, WaitTimeoutResult};
pub use self::fair_mutex::{FairMutex, FairMutexGuard, MappedFairMutexGuard, const_fair_mutex};
pub use self::mutex::{MappedMutexGuard, Mutex, MutexGuard, const_mutex};
pub use self::once::{Once, OnceState};
pub use self::raw_fair_mutex::RawFairMutex;
pub use self::raw_mutex::RawMutex;
pub use self::raw_rwlock::RawRwLock;
pub use self::remutex::{
    MappedReentrantMutexGuard, RawThreadId, ReentrantMutex, ReentrantMutexGuard, const_reentrant_mutex,
};
pub use self::rwlock::{
    MappedRwLockReadGuard, MappedRwLockWriteGuard, RwLock, RwLockReadGuard, RwLockUpgradableReadGuard,
    RwLockWriteGuard, const_rwlock,
};

// The `Arc`-based guards are plain re-exports of the generic `lock_api` guard types (gated on
// `arc_lock`), placed here at the crate root exactly as `parking_lot` does in its `lib.rs`.
#[cfg(feature = "arc_lock")]
pub use ::lock_api::{
    ArcMutexGuard, ArcReentrantMutexGuard, ArcRwLockReadGuard, ArcRwLockUpgradableReadGuard, ArcRwLockWriteGuard,
};

// Re-export `lock_api` to mirror `parking_lot`'s own `pub use lock_api;`, so code that refers to
// `parking_lot::lock_api::...` continues to resolve under the Shuttle feature. This is the same
// `lock_api` instance the type aliases are built from, so the types unify.
pub use ::lock_api;
