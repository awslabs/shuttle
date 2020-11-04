//! Shuttle's implementation of `std::sync`.

mod condvar;
mod mutex;
mod rwlock;

pub use condvar::Condvar;

pub use mutex::Mutex;
pub use mutex::MutexGuard;

pub use rwlock::RwLock;
pub use rwlock::RwLockReadGuard;
pub use rwlock::RwLockWriteGuard;
