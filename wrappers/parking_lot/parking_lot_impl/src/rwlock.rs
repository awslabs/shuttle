//! An asynchronous reader-writer lock.

// This implementation is adapted from the one in shuttle-tokio

use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness, TryAcquireError};
use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::{fmt, ops};
use tracing::trace;

const MAX_READERS: usize = usize::MAX >> 3;

/// A reader-writer lock
#[derive(Debug)]
pub struct RwLock<T: ?Sized> {
    // maximum number of concurrent readers
    max_readers: usize,

    //semaphore to coordinate read and write access to T
    sem: BatchSemaphore,

    //inner data T
    inner: UnsafeCell<T>,
}

impl<T> RwLock<T> {
    /// Creates a new instance of an `RwLock<T>` which is unlocked.
    pub const fn new(value: T) -> Self {
        Self::with_max_readers(value, MAX_READERS)
    }

    /// Creates a new instance of an `RwLock<T>` which is unlocked
    /// and allows a maximum of `max_readers` concurrent readers.
    const fn with_max_readers(value: T, max_readers: usize) -> Self {
        let sem = BatchSemaphore::const_new(max_readers, Fairness::StrictlyFair);
        let rwlock = RwLock {
            max_readers,
            sem,
            inner: UnsafeCell::new(value),
        };
        rwlock
    }
}

impl<T: ?Sized> RwLock<T> {
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
        trace!("parking_lot rwlock {:p} acquiring read lock", self);
        self.sem.acquire_blocking(1).unwrap_or_else(|_| {
            // The semaphore was closed. but, we never explicitly close it, and we have a
            // handle to it through the Arc, which means that this can never happen.
            if !std::thread::panicking() {
                unreachable!()
            }
        });

        trace!("parking_lot rwlock {:p} acquired", self);

        RwLockReadGuard {
            sem: &self.sem,
            data: self.inner.get(),
            _p: PhantomData,
        }
    }

    /// Attempts to acquire this `RwLock` with shared read access.
    ///
    /// If the access couldn't be acquired immediately, returns [`TryLockError`].
    /// Otherwise, an RAII guard is returned which will release read access
    /// when dropped.
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        match self.sem.try_acquire(1) {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => return None,
            Err(TryAcquireError::Closed) => {
                if !std::thread::panicking() {
                    unreachable!()
                }
                return None;
            }
        }

        trace!("parking_lot rwlock {:p} try_read acquired ReadGuard", self);

        Some(RwLockReadGuard {
            sem: &self.sem,
            data: self.inner.get(),
            _p: PhantomData,
        })
    }

    /// Locks this `RwLock` with exclusive write access, blocking the current
    /// thread until it can be acquired.
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        trace!("parking_lot rwlock {:p} acquiring write lock", self);
        self.sem.acquire_blocking(1).unwrap_or_else(|_| {
            // The semaphore was closed. but, we never explicitly close it, and we have a
            // handle to it through the Arc, which means that this can never happen.
            if !std::thread::panicking() {
                unreachable!()
            }
        });

        trace!("parking_lot rwlock {:p} acquired WriteGuard", self);
        RwLockWriteGuard {
            permits_acquired: self.max_readers,
            data: self.inner.get(),
            sem: &self.sem,
            _p: PhantomData,
        }
    }

    /// Attempts to acquire this `RwLock` with exclusive write access.
    ///
    /// If the access couldn't be acquired immediately, returns [`TryLockError`].
    /// Otherwise, an RAII guard is returned which will release write access
    /// when dropped.
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        tracing::trace!("parking_lot rwlock {:p} try_write acquired WriteGuard", self,);

        match self.sem.try_acquire(1) {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => return None,
            Err(TryAcquireError::Closed) => {
                if !std::thread::panicking() {
                    unreachable!()
                }
                return None;
            }
        }

        tracing::trace!("parking_lot rwlock {:p} try_write acquired WriteGuard", self,);

        Some(RwLockWriteGuard {
            permits_acquired: self.max_readers,
            sem: &self.sem,
            data: self.inner.get(),
            _p: PhantomData,
        })
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `RwLock` mutably, no actual locking needs to
    /// take place -- the mutable borrow statically guarantees no locks exist.
    pub fn get_mut(&mut self) -> &mut T {
        unsafe {
            // Safety: This is https://github.com/rust-lang/rust/pull/76936
            &mut *self.inner.get()
        }
    }

    /// Consumes the lock, returning the underlying data.
    pub fn into_inner(self) -> T
    where
        T: Sized,
    {
        self.inner.into_inner()
    }
}

impl<T> From<T> for RwLock<T> {
    fn from(s: T) -> Self {
        Self::new(s)
    }
}

impl<T> Default for RwLock<T>
where
    T: Default,
{
    fn default() -> Self {
        Self::new(T::default())
    }
}

// As long as T: Send + Sync, it's fine to send and share RwLock<T> between threads.
// If T were not Send, sending and sharing a RwLock<T> would be bad, since you can access T through
// RwLock<T>.
unsafe impl<T> Send for RwLock<T> where T: ?Sized + Send {}
unsafe impl<T> Sync for RwLock<T> where T: ?Sized + Send + Sync {}
// NB: These impls need to be explicit since we're storing a raw pointer.
// Safety: Stores a raw pointer to `T`, so if `T` is `Sync`, the lock guard over
// `T` is `Send`.
unsafe impl<T> Send for RwLockReadGuard<'_, T> where T: ?Sized + Sync {}
unsafe impl<T> Sync for RwLockReadGuard<'_, T> where T: ?Sized + Send + Sync {}

unsafe impl<T> Sync for RwLockWriteGuard<'_, T> where T: ?Sized + Send + Sync {}

// Safety: Stores a raw pointer to `T`, so if `T` is `Sync`, the lock guard over
// `T` is `Send` - but since this is also provides mutable access, we need to
// make sure that `T` is `Send` since its value can be sent across thread
// boundaries.
unsafe impl<T> Send for RwLockWriteGuard<'_, T> where T: ?Sized + Send + Sync {}

/// RAII structure used to release the shared read access of a lock when
/// dropped.
///
/// This structure is created by the [`read`] method on
/// [`RwLock`].
///
/// [`read`]: method@crate::sync::RwLock::read
/// [`RwLock`]: struct@crate::sync::RwLock
pub struct RwLockReadGuard<'a, T: ?Sized> {
    sem: &'a BatchSemaphore,
    data: *const T,
    _p: PhantomData<&'a T>,
}

impl<T: ?Sized> ops::Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.data }
    }
}

impl<T: ?Sized> fmt::Debug for RwLockReadGuard<'_, T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized> Drop for RwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        self.sem.release(1);
    }
}

/// RAII structure used to release the exclusive write access of a lock when
/// dropped.
pub struct RwLockWriteGuard<'a, T: ?Sized> {
    permits_acquired: usize,
    sem: &'a BatchSemaphore,
    data: *mut T,
    _p: PhantomData<&'a mut T>,
}

impl<'a, T: ?Sized> RwLockWriteGuard<'a, T> {
    /// Atomically downgrades a write lock into a read lock without allowing
    /// any writers to take exclusive access of the lock in the meantime.
    ///
    /// **Note:** This won't *necessarily* allow any additional readers to acquire
    /// locks, since [`RwLock`] is fair and it is possible that a writer is next
    /// in line.
    pub fn downgrade(self) -> RwLockReadGuard<'a, T> {
        let RwLockWriteGuard { sem, data, .. } = self;
        let to_release = self.permits_acquired - 1;

        tracing::trace!("rwlock {:p} downgrading to ReadGuard", &self);

        // NB: Forget to avoid drop impl from being called.
        std::mem::forget(self);

        // Release all but one of the permits held by the write guard
        sem.release(to_release);

        RwLockReadGuard {
            sem,
            data,
            _p: PhantomData,
        }
    }
}

impl<T: ?Sized> ops::Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.data }
    }
}

impl<T: ?Sized> ops::DerefMut for RwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.data }
    }
}

impl<T: ?Sized> fmt::Debug for RwLockWriteGuard<'_, T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized> Drop for RwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        self.sem.release(self.permits_acquired);
    }
}
