//! This crate contains Shuttle's internal implementations of the `parking_lot` crate.
//! Do not depend on this crate directly. Use the `shuttle-parking_lot` crate, which conditionally
//! exposes these implementations with the `shuttle` feature or the original crate without it.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//!
//! [`parking_lot`]: <https://crates.io/crates/parking_lot>

use shuttle::sync;

// TODO: All the `expect`s below are wrong — it is possible to poison a Shuttle mutex.
//       `clear_poison` should be added and used to avoid the `expect`.
// TODO: Shuttle should be split up into a "core" crate which offers a more versatile
//       Mutex which is parametric on poisoning (and this crate would initialize it as non-poisonable)

#[derive(Default, Debug)]
pub struct Mutex<T: ?Sized> {
    inner: sync::Mutex<T>,
}

pub use sync::{MutexGuard, RwLockReadGuard, RwLockWriteGuard};

impl<T> Mutex<T> {
    /// Creates a new mutex in an unlocked state ready for use.
    pub fn new(value: T) -> Self {
        Self {
            inner: sync::Mutex::new(value),
        }
    }

    /// Consumes this mutex, returning the underlying data.
    #[inline]
    pub fn into_inner(self) -> T {
        self.inner
            .into_inner()
            .expect("This shouldn't happen as there is no way to poision a Shuttle Mutex.")
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Acquires a mutex, blocking the current thread until it is able to do so.
    ///
    /// This function will block the local thread until it is available to acquire
    /// the mutex. Upon returning, the thread is the only thread with the mutex
    /// held. An RAII guard is returned to allow scoped unlock of the lock. When
    /// the guard goes out of scope, the mutex will be unlocked.
    ///
    /// Attempts to lock a mutex in the thread which already holds the lock will
    /// result in a deadlock.
    #[inline]
    pub fn lock(&self) -> sync::MutexGuard<'_, T> {
        self.inner
            .lock()
            .expect("This shouldn't happen as there is no way to poision a Shuttle Mutex.")
    }

    /// Attempts to acquire this lock.
    ///
    /// If the lock could not be acquired at this time, then `None` is returned.
    /// Otherwise, an RAII guard is returned. The lock will be unlocked when the
    /// guard is dropped.
    #[inline]
    pub fn try_lock(&self) -> Option<sync::MutexGuard<'_, T>> {
        self.inner.try_lock().ok()
    }
}

/// A reader-writer lock
#[derive(Debug, Default)]
pub struct RwLock<T: ?Sized> {
    inner: sync::RwLock<T>,
}

impl<T> RwLock<T> {
    /// Creates a new instance of an `RwLock<T>` which is unlocked.
    pub fn new(value: T) -> Self {
        Self {
            inner: sync::RwLock::new(value),
        }
    }

    /// Consumes this `RwLock`, returning the underlying data.
    #[inline]
    pub fn into_inner(self) -> T {
        self.inner
            .into_inner()
            .expect("This shouldn't happen as there is no way to poision a Shuttle RwLock.")
    }
}

impl<T: ?Sized> RwLock<T> {
    /// Locks this `RwLock` with shared read access, blocking the current thread
    /// until it can be acquired.
    ///
    /// The calling thread will be blocked until there are no more writers which
    /// hold the lock. There may be other readers currently inside the lock when
    /// this method returns.
    ///
    /// Note that attempts to recursively acquire a read lock on a `RwLock` when
    /// the current thread already holds one may result in a deadlock.
    ///
    /// Returns an RAII guard which will release this thread's shared access
    /// once it is dropped.
    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        self.inner
            .read()
            .expect("This shouldn't happen as there is no way to poision a Shuttle RwLock.")
    }

    /// Attempts to acquire this `RwLock` with shared read access.
    ///
    /// If the access could not be granted at this time, then `None` is returned.
    /// Otherwise, an RAII guard is returned which will release the shared access
    /// when it is dropped.
    ///
    /// This function does not block.
    #[inline]
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        self.inner.try_read().ok()
    }

    /// Locks this `RwLock` with exclusive write access, blocking the current
    /// thread until it can be acquired.
    ///
    /// This function will not return while other writers or other readers
    /// currently have access to the lock.
    ///
    /// Returns an RAII guard which will drop the write access of this `RwLock`
    /// when dropped.
    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.inner
            .write()
            .expect("This shouldn't happen as there is no way to poision a Shuttle RwLock.")
    }

    /// Attempts to lock this `RwLock` with exclusive write access.
    ///
    /// If the lock could not be acquired at this time, then `None` is returned.
    /// Otherwise, an RAII guard is returned which will release the lock when
    /// it is dropped.
    ///
    /// This function does not block.
    #[inline]
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        self.inner.try_write().ok()
    }
}
