pub mod atomic;
mod barrier;
mod condvar;
pub mod mpsc;
mod mutex;
mod once;
mod rwlock;

pub use barrier::{Barrier, BarrierWaitResult};
pub use condvar::{Condvar, WaitTimeoutResult};

pub use mutex::Mutex;
pub use mutex::MutexGuard;

pub use once::Once;
pub use once::OnceState;

pub use rwlock::RwLock;
pub use rwlock::RwLockReadGuard;
pub use rwlock::RwLockWriteGuard;

pub use std::sync::{LockResult, PoisonError, TryLockError, TryLockResult};

// We re-export `std::sync::Arc` rather than modeling it in Shuttle. `Arc` is typically used to
// share data across threads, not to synchronize, so modeling its reference-count operations
// (which would require a context switch on every count modification) would significantly hurt
// performance for little gain. This means Shuttle misses some behaviors, e.g. around `Weak`
// upgrade/downgrade races. If we ever want to support those, it should be opt-in or configurable
// rather than the default.
pub use std::sync::{Arc, Weak};

pub use shuttle_engine::sync_types::{ResourceSignature, ResourceType};
