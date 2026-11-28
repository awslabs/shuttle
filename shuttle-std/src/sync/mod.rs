pub mod atomic;
mod barrier;
mod condvar;
mod lazy_lock;
pub mod mpsc;
mod mutex;
mod once;
mod once_lock;
mod rwlock;

pub use barrier::{Barrier, BarrierWaitResult};
pub use condvar::{Condvar, WaitTimeoutResult};

pub use lazy_lock::LazyLock;

pub use mutex::Mutex;
pub use mutex::MutexGuard;

pub use once::Once;
pub use once::OnceState;

pub use once_lock::OnceLock;

pub use rwlock::RwLock;
pub use rwlock::RwLockReadGuard;
pub use rwlock::RwLockWriteGuard;

pub use std::sync::{LockResult, PoisonError, TryLockError, TryLockResult};

// TODO implement true support for `Arc`
pub use std::sync::{Arc, Weak};

pub use shuttle_engine::sync_types::{ResourceSignature, ResourceType};
