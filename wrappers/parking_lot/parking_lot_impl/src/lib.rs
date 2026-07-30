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
//!
//! This layering means the entire `parking_lot` guard surface — `Arc` guards, mapped guards,
//! upgradable guards — is available under Shuttle without reimplementing it by hand.

mod mutex;
mod raw_mutex;
mod raw_rwlock;
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
pub use self::raw_mutex::RawMutex;
pub use self::raw_rwlock::RawRwLock;

pub use self::mutex::{MappedMutexGuard, Mutex, MutexGuard, const_mutex};
pub use self::rwlock::{
    MappedRwLockReadGuard, MappedRwLockWriteGuard, RwLock, RwLockReadGuard, RwLockUpgradableReadGuard,
    RwLockWriteGuard, const_rwlock,
};

// The `Arc`-based guards are plain re-exports of the generic `lock_api` guard types (gated on
// `arc_lock`), placed here at the crate root exactly as `parking_lot` does in its `lib.rs`.
#[cfg(feature = "arc_lock")]
pub use ::lock_api::{ArcMutexGuard, ArcRwLockReadGuard, ArcRwLockUpgradableReadGuard, ArcRwLockWriteGuard};

// Re-export `lock_api` to mirror `parking_lot`'s own `pub use lock_api;`, so code that refers to
// `parking_lot::lock_api::...` continues to resolve under the Shuttle feature. This is the same
// `lock_api` instance the type aliases are built from, so the types unify.
pub use ::lock_api;
