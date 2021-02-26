//! Shuttle's implementation of `std::sync`.

mod barrier;
mod condvar;

/// Multi-producer, single-consumer FIFO queue communication primitives.
pub mod mpsc;
mod mutex;
mod rwlock;

pub use barrier::{Barrier, BarrierWaitResult};
pub use condvar::{Condvar, WaitTimeoutResult};

pub use mutex::Mutex;
pub use mutex::MutexGuard;

pub use rwlock::RwLock;
pub use rwlock::RwLockReadGuard;
pub use rwlock::RwLockWriteGuard;

// TODO implement true support for `Arc`
pub use std::sync::Arc;
