//! Shuttle's implementation of `std::sync`.

mod condvar;
mod mpsc;
mod mutex;
mod rwlock;

pub use condvar::Condvar;

pub use mpsc::{channel, sync_channel};
pub use mpsc::{Receiver, RecvError, Sender, SyncSender, TryRecvError};

pub use mutex::Mutex;
pub use mutex::MutexGuard;

pub use rwlock::RwLock;
pub use rwlock::RwLockReadGuard;
pub use rwlock::RwLockWriteGuard;
