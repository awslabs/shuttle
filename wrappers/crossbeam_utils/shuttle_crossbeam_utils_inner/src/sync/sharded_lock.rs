//! `ShardedLock`, on Shuttle's `RwLock`.

use shuttle::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::{LockResult, PoisonError, TryLockError, TryLockResult};

/// A sharded reader-writer lock.
///
/// This type is like [`RwLock`](std::sync::RwLock), except read operations are faster and write
/// operations are slower.
///
/// Under Shuttle it is Shuttle's `RwLock`, which is what the sharding is an optimization of: a
/// reader blocks only while a writer holds the lock, the scheduler decides who gets the lock, and
/// a task that panics while holding it poisons the lock. There is nothing to be gained from
/// modelling the shards, since they are not observable, and one lock explores fewer schedules.
///
/// # Differences from `crossbeam_utils::sync::ShardedLock`
///
/// A read guard is tied to the lock's own reader count, so the real type's trick of keeping a
/// shard's guard alive is not observable here either.
pub struct ShardedLock<T: ?Sized> {
    inner: RwLock<T>,
}

// Safety: the bounds are crossbeam's, and Shuttle's `RwLock` carries the same ones.
unsafe impl<T: ?Sized + Send> Send for ShardedLock<T> {}
unsafe impl<T: ?Sized + Send + Sync> Sync for ShardedLock<T> {}

impl<T> ShardedLock<T> {
    /// Creates a new sharded reader-writer lock.
    ///
    /// Not a `const fn`, as crossbeam's is not.
    pub fn new(value: T) -> ShardedLock<T> {
        ShardedLock {
            inner: RwLock::new(value),
        }
    }

    /// Consumes this lock, returning the underlying data.
    ///
    /// # Errors
    ///
    /// This function will return an error if the lock is poisoned. A lock gets poisoned when a
    /// write operation panics.
    pub fn into_inner(self) -> LockResult<T> {
        self.inner.into_inner()
    }
}

impl<T: ?Sized> ShardedLock<T> {
    /// Returns `true` if the lock is poisoned.
    ///
    /// If another thread can still access the lock, it may become poisoned at any time. A `false`
    /// return value should not be trusted for program correctness without additional
    /// synchronization.
    pub fn is_poisoned(&self) -> bool {
        self.inner.is_poisoned()
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the lock mutably, no actual locking needs to take place.
    ///
    /// # Errors
    ///
    /// This function will return an error if the lock is poisoned. A lock gets poisoned when a
    /// write operation panics.
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        self.inner.get_mut()
    }

    /// Attempts to acquire this lock with shared read access.
    ///
    /// If the access could not be granted at this time, an error is returned. Otherwise, a guard
    /// is returned which will release the shared access when it is dropped. This method does not
    /// provide any guarantees with respect to the ordering of whether contentious readers or
    /// writers will acquire the lock first.
    ///
    /// # Errors
    ///
    /// This function will return an error if the lock is poisoned. A lock gets poisoned when a
    /// write operation panics.
    pub fn try_read(&self) -> TryLockResult<ShardedLockReadGuard<'_, T>> {
        match self.inner.try_read() {
            Ok(inner) => Ok(ShardedLockReadGuard { inner }),
            Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
            Err(TryLockError::Poisoned(err)) => Err(TryLockError::Poisoned(PoisonError::new(ShardedLockReadGuard {
                inner: err.into_inner(),
            }))),
        }
    }

    /// Locks with shared read access, blocking the current thread until it can be acquired.
    ///
    /// The calling thread will be blocked until there are no more writers which hold the lock.
    /// There may be other readers currently inside the lock when this method returns. This method
    /// does not provide any guarantees with respect to the ordering of whether contentious readers
    /// or writers will acquire the lock first.
    ///
    /// Returns a guard which will release the shared access when dropped.
    ///
    /// # Errors
    ///
    /// This function will return an error if the lock is poisoned. A lock gets poisoned when a
    /// write operation panics.
    pub fn read(&self) -> LockResult<ShardedLockReadGuard<'_, T>> {
        match self.inner.read() {
            Ok(inner) => Ok(ShardedLockReadGuard { inner }),
            Err(err) => Err(PoisonError::new(ShardedLockReadGuard {
                inner: err.into_inner(),
            })),
        }
    }

    /// Attempts to acquire this lock with exclusive write access.
    ///
    /// If the access could not be granted at this time, an error is returned. Otherwise, a guard
    /// is returned which will release the exclusive access when it is dropped. This method does
    /// not provide any guarantees with respect to the ordering of whether contentious readers or
    /// writers will acquire the lock first.
    ///
    /// # Errors
    ///
    /// This function will return an error if the lock is poisoned. A lock gets poisoned when a
    /// write operation panics.
    pub fn try_write(&self) -> TryLockResult<ShardedLockWriteGuard<'_, T>> {
        match self.inner.try_write() {
            Ok(inner) => Ok(ShardedLockWriteGuard { inner }),
            Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
            Err(TryLockError::Poisoned(err)) => Err(TryLockError::Poisoned(PoisonError::new(ShardedLockWriteGuard {
                inner: err.into_inner(),
            }))),
        }
    }

    /// Locks with exclusive write access, blocking the current thread until it can be acquired.
    ///
    /// The calling thread will be blocked until there are no more writers which hold the lock.
    /// There may be other readers currently inside the lock when this method returns. This method
    /// does not provide any guarantees with respect to the ordering of whether contentious readers
    /// or writers will acquire the lock first.
    ///
    /// Returns a guard which will release the exclusive access when dropped.
    ///
    /// # Errors
    ///
    /// This function will return an error if the lock is poisoned. A lock gets poisoned when a
    /// write operation panics.
    pub fn write(&self) -> LockResult<ShardedLockWriteGuard<'_, T>> {
        match self.inner.write() {
            Ok(inner) => Ok(ShardedLockWriteGuard { inner }),
            Err(err) => Err(PoisonError::new(ShardedLockWriteGuard {
                inner: err.into_inner(),
            })),
        }
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for ShardedLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_read() {
            Ok(guard) => f.debug_struct("ShardedLock").field("data", &&*guard).finish(),
            Err(TryLockError::Poisoned(err)) => f.debug_struct("ShardedLock").field("data", &&**err.get_ref()).finish(),
            Err(TryLockError::WouldBlock) => {
                struct LockedPlaceholder;
                impl fmt::Debug for LockedPlaceholder {
                    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("<locked>")
                    }
                }
                f.debug_struct("ShardedLock").field("data", &LockedPlaceholder).finish()
            }
        }
    }
}

impl<T: Default> Default for ShardedLock<T> {
    fn default() -> ShardedLock<T> {
        ShardedLock::new(Default::default())
    }
}

impl<T> From<T> for ShardedLock<T> {
    fn from(t: T) -> Self {
        ShardedLock::new(t)
    }
}

/// A guard used to release the shared read access of a [`ShardedLock`] when dropped.
#[clippy::has_significant_drop]
pub struct ShardedLockReadGuard<'a, T: ?Sized> {
    inner: RwLockReadGuard<'a, T>,
}

// Safety: as crossbeam's, and Shuttle's guard carries the same bound.
unsafe impl<T: ?Sized + Sync> Sync for ShardedLockReadGuard<'_, T> {}

impl<T: ?Sized> Deref for ShardedLockReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T: fmt::Debug> fmt::Debug for ShardedLockReadGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShardedLockReadGuard")
            .field("lock", &&*self.inner)
            .finish()
    }
}

impl<T: ?Sized + fmt::Display> fmt::Display for ShardedLockReadGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

/// A guard used to release the exclusive write access of a [`ShardedLock`] when dropped.
#[clippy::has_significant_drop]
pub struct ShardedLockWriteGuard<'a, T: ?Sized> {
    inner: RwLockWriteGuard<'a, T>,
}

// Safety: as crossbeam's, and Shuttle's guard carries the same bound.
unsafe impl<T: ?Sized + Sync> Sync for ShardedLockWriteGuard<'_, T> {}

impl<T: ?Sized> Deref for ShardedLockWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T: ?Sized> DerefMut for ShardedLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T: fmt::Debug> fmt::Debug for ShardedLockWriteGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShardedLockWriteGuard")
            .field("lock", &&*self.inner)
            .finish()
    }
}

impl<T: ?Sized + fmt::Display> fmt::Display for ShardedLockWriteGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}
