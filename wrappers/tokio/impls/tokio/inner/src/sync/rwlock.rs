//! An asynchronous reader-writer lock.

use crate::sync::TryLockError;
use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness, TryAcquireError};
use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::sync::Arc;
use std::{fmt, ops};
use tracing::trace;

const MAX_READERS: usize = usize::MAX >> 3;

/// An asynchronous reader-writer lock.
///
/// This type of lock allows a number of readers or at most one writer at any
/// point in time. The write portion of this lock typically allows modification
/// of the underlying data (exclusive access) and the read portion of this lock
/// typically allows for read-only access (shared access).
///
/// In comparison, a `Mutex` does not distinguish between readers or writers
/// that acquire the lock, therefore causing any tasks waiting for the lock to
/// become available to yield. An `RwLock` will allow any number of readers to
/// acquire the lock as long as a writer is not holding the lock.
#[derive(Debug)]
pub struct RwLock<T: ?Sized> {
    // maximum number of concurrent readers
    max_readers: usize,

    //semaphore to coordinate read and write access to T
    sem: BatchSemaphore,

    //inner data T
    inner: UnsafeCell<T>,
}

impl<T: ?Sized> RwLock<T> {
    /// Creates a new instance of an `RwLock<T>` which is unlocked.
    pub fn new(value: T) -> Self
    where
        T: Sized,
    {
        Self::with_max_readers(value, MAX_READERS)
    }

    /// Creates a new instance of an `RwLock<T>` which is unlocked
    /// and allows a maximum of `max_readers` concurrent readers.
    pub fn with_max_readers(value: T, max_readers: usize) -> Self
    where
        T: Sized,
    {
        assert!(
            max_readers <= MAX_READERS,
            "a RwLock may not be created with more than {MAX_READERS} readers"
        );
        let sem = BatchSemaphore::new(max_readers, Fairness::StrictlyFair);
        let rwlock = RwLock {
            max_readers,
            sem,
            inner: UnsafeCell::new(value),
        };
        trace!("initialized RwLock {:p} with {} permits", &rwlock, max_readers);
        rwlock
    }

    /// Locks this `RwLock` with shared read access, causing the current task
    /// to yield until the lock has been acquired.
    ///
    /// The calling task will yield until there are no writers which hold the
    /// lock. There may be other readers inside the lock when the task resumes.
    ///
    /// Returns an RAII guard which will drop this read access of the `RwLock`
    /// when dropped.
    pub async fn read(&self) -> RwLockReadGuard<'_, T> {
        let inner = self.sem.acquire(1);
        inner.await.unwrap_or_else(|_| {
            // The semaphore was closed. but, we never explicitly close it, and we have a
            // handle to it through the Arc, which means that this can never happen.
            if !std::thread::panicking() {
                unreachable!()
            }
        });

        trace!("rwlock {:p} acquired ReadGuard", self);
        RwLockReadGuard {
            sem: &self.sem,
            data: self.inner.get(),
            _p: PhantomData,
        }
    }

    /// Blockingly locks this `RwLock` with shared read access.
    ///
    /// This method is intended for use cases where you
    /// need to use this rwlock in asynchronous code as well as in synchronous code.
    pub fn blocking_read(&self) -> RwLockReadGuard<'_, T> {
        shuttle::future::block_on(self.read())
    }

    /// Locks this `RwLock` with shared read access, causing the current task
    /// to yield until the lock has been acquired.
    ///
    /// The calling task will yield until there are no writers which hold the
    /// lock. There may be other readers inside the lock when the task resumes.
    ///
    /// This method is identical to [`RwLock::read`], except that the returned
    /// guard references the `RwLock` with an [`Arc`] rather than by borrowing
    /// it. Therefore, the `RwLock` must be wrapped in an `Arc` to call this
    /// method, and the guard will live for the `'static` lifetime, as it keeps
    /// the `RwLock` alive by holding an `Arc`.
    ///
    /// Returns an RAII guard which will drop this read access of the `RwLock`
    /// when dropped.
    pub async fn read_owned(self: Arc<Self>) -> OwnedRwLockReadGuard<T> {
        let inner = self.sem.acquire(1);
        inner.await.unwrap_or_else(|_| {
            // The semaphore was closed. but, we never explicitly close it, and we have a
            // handle to it through the Arc, which means that this can never happen.
            if !std::thread::panicking() {
                unreachable!()
            }
        });

        trace!("rwlock {:p} acquired OwnedReadGuard", self);
        OwnedRwLockReadGuard {
            data: self.inner.get(),
            lock: ManuallyDrop::new(self),
            _p: PhantomData,
        }
    }

    /// Attempts to acquire this `RwLock` with shared read access.
    ///
    /// If the access couldn't be acquired immediately, returns [`TryLockError`].
    /// Otherwise, an RAII guard is returned which will release read access
    /// when dropped.
    pub fn try_read(&self) -> Result<RwLockReadGuard<'_, T>, TryLockError> {
        match self.sem.try_acquire(1) {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => return Err(TryLockError(())),
            Err(TryAcquireError::Closed) => {
                if !std::thread::panicking() {
                    unreachable!()
                }
            }
        }

        trace!("rwlock {:p} try_read acquired ReadGuard", self);
        Ok(RwLockReadGuard {
            sem: &self.sem,
            data: self.inner.get(),
            _p: PhantomData,
        })
    }

    /// Attempts to acquire this `RwLock` with shared read access.
    ///
    /// If the access couldn't be acquired immediately, returns [`TryLockError`].
    /// Otherwise, an RAII guard is returned which will release read access
    /// when dropped.
    ///
    /// This method is identical to [`RwLock::try_read`], except that the
    /// returned guard references the `RwLock` with an [`Arc`] rather than by
    /// borrowing it. Therefore, the `RwLock` must be wrapped in an `Arc` to
    /// call this method, and the guard will live for the `'static` lifetime,
    /// as it keeps the `RwLock` alive by holding an `Arc`.
    pub fn try_read_owned(self: Arc<Self>) -> Result<OwnedRwLockReadGuard<T>, TryLockError> {
        match self.sem.try_acquire(1) {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => return Err(TryLockError(())),
            Err(TryAcquireError::Closed) => {
                if !std::thread::panicking() {
                    unreachable!()
                }
            }
        }

        trace!("rwlock {:p} try_read acquired OwnedReadGuard", self);
        Ok(OwnedRwLockReadGuard {
            data: self.inner.get(),
            lock: ManuallyDrop::new(self),
            _p: PhantomData,
        })
    }

    /// Locks this `RwLock` with exclusive write access, causing the current
    /// task to yield until the lock has been acquired.
    ///
    /// The calling task will yield while other writers or readers currently
    /// have access to the lock.
    ///
    /// Returns an RAII guard which will drop the write access of this `RwLock`
    /// when dropped.
    pub async fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.sem.acquire(self.max_readers).await.unwrap_or_else(|_| {
            if !std::thread::panicking() {
                unreachable!()
            }
        });

        trace!("rwlock {:p} acquired WriteGuard", self);
        RwLockWriteGuard {
            permits_acquired: self.max_readers,
            data: self.inner.get(),
            sem: &self.sem,
            _p: PhantomData,
        }
    }

    /// Blockingly locks this `RwLock` with exclusive write access.
    ///
    /// This method is intended for use cases where you
    /// need to use this rwlock in asynchronous code as well as in synchronous code.
    pub fn blocking_write(&self) -> RwLockWriteGuard<'_, T> {
        shuttle::future::block_on(self.write())
    }

    /// Locks this `RwLock` with exclusive write access, causing the current
    /// task to yield until the lock has been acquired.
    ///
    /// The calling task will yield while other writers or readers currently
    /// have access to the lock.
    ///
    /// This method is identical to [`RwLock::write`], except that the returned
    /// guard references the `RwLock` with an [`Arc`] rather than by borrowing
    /// it. Therefore, the `RwLock` must be wrapped in an `Arc` to call this
    /// method, and the guard will live for the `'static` lifetime, as it keeps
    /// the `RwLock` alive by holding an `Arc`.
    pub async fn write_owned(self: Arc<Self>) -> OwnedRwLockWriteGuard<T> {
        self.sem.acquire(self.max_readers).await.unwrap_or_else(|_| {
            // The semaphore was closed. but, we never explicitly close it, and we have a
            // handle to it through the Arc, which means that this can never happen.
            if !std::thread::panicking() {
                unreachable!()
            }
        });

        tracing::trace!("rwlock {:p} acquired OwnedWriteGuard", self,);
        OwnedRwLockWriteGuard {
            permits_acquired: self.max_readers,
            data: self.inner.get(),
            lock: ManuallyDrop::new(self),
            _p: PhantomData,
        }
    }

    /// Attempts to acquire this `RwLock` with exclusive write access.
    ///
    /// If the access couldn't be acquired immediately, returns [`TryLockError`].
    /// Otherwise, an RAII guard is returned which will release write access
    /// when dropped.
    pub fn try_write(&self) -> Result<RwLockWriteGuard<'_, T>, TryLockError> {
        match self.sem.try_acquire(self.max_readers) {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => return Err(TryLockError(())),
            Err(TryAcquireError::Closed) => {
                if !std::thread::panicking() {
                    unreachable!()
                }
            }
        }

        tracing::trace!("rwlock {:p} try_write acquired WriteGuard", self,);
        Ok(RwLockWriteGuard {
            permits_acquired: self.max_readers,
            sem: &self.sem,
            data: self.inner.get(),
            _p: PhantomData,
        })
    }

    /// Attempts to acquire this `RwLock` with exclusive write access.
    ///
    /// If the access couldn't be acquired immediately, returns [`TryLockError`].
    /// Otherwise, an RAII guard is returned which will release write access
    /// when dropped.
    ///
    /// This method is identical to [`RwLock::try_write`], except that the
    /// returned guard references the `RwLock` with an [`Arc`] rather than by
    /// borrowing it. Therefore, the `RwLock` must be wrapped in an `Arc` to
    /// call this method, and the guard will live for the `'static` lifetime,
    /// as it keeps the `RwLock` alive by holding an `Arc`.
    pub fn try_write_owned(self: Arc<Self>) -> Result<OwnedRwLockWriteGuard<T>, TryLockError> {
        match self.sem.try_acquire(self.max_readers) {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => return Err(TryLockError(())),
            Err(TryAcquireError::Closed) => {
                if !std::thread::panicking() {
                    unreachable!()
                }
            }
        }

        tracing::trace!("rwlock {:p} try_write acquired OwnedWriteGuard", self,);
        Ok(OwnedRwLockWriteGuard {
            permits_acquired: self.max_readers,
            data: self.inner.get(),
            lock: ManuallyDrop::new(self),
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
// T is required to be `Send` because an OwnedRwLockReadGuard can be used to drop the value held in
// the RwLock, unlike RwLockReadGuard.
unsafe impl<T, U> Send for OwnedRwLockReadGuard<T, U>
where
    T: ?Sized + Send + Sync,
    U: ?Sized + Sync,
{
}
unsafe impl<T, U> Sync for OwnedRwLockReadGuard<T, U>
where
    T: ?Sized + Send + Sync,
    U: ?Sized + Send + Sync,
{
}
unsafe impl<T> Sync for RwLockWriteGuard<'_, T> where T: ?Sized + Send + Sync {}
unsafe impl<T> Sync for OwnedRwLockWriteGuard<T> where T: ?Sized + Send + Sync {}

// Safety: Stores a raw pointer to `T`, so if `T` is `Sync`, the lock guard over
// `T` is `Send` - but since this is also provides mutable access, we need to
// make sure that `T` is `Send` since its value can be sent across thread
// boundaries.
unsafe impl<T> Send for RwLockWriteGuard<'_, T> where T: ?Sized + Send + Sync {}
unsafe impl<T> Send for OwnedRwLockWriteGuard<T> where T: ?Sized + Send + Sync {}

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

/// Owned RAII structure used to release the shared read access of a lock when
/// dropped.
///
/// This structure is created by the [`read_owned`] method on
/// [`RwLock`].
///
/// [`read_owned`]: method@crate::sync::RwLock::read_owned
/// [`RwLock`]: struct@crate::sync::RwLock
pub struct OwnedRwLockReadGuard<T: ?Sized, U: ?Sized = T> {
    // ManuallyDrop allows us to destructure into this field without running the destructor.
    lock: ManuallyDrop<Arc<RwLock<T>>>,
    data: *const U,
    _p: PhantomData<T>,
}

impl<T: ?Sized, U: ?Sized> ops::Deref for OwnedRwLockReadGuard<T, U> {
    type Target = U;

    fn deref(&self) -> &U {
        unsafe { &*self.data }
    }
}

impl<T: ?Sized, U: ?Sized> fmt::Debug for OwnedRwLockReadGuard<T, U>
where
    U: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized, U: ?Sized> Drop for OwnedRwLockReadGuard<T, U> {
    fn drop(&mut self) {
        self.lock.sem.release(1);
        unsafe { ManuallyDrop::drop(&mut self.lock) };
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

/// Owned RAII structure used to release the exclusive write access of a lock when
/// dropped.
pub struct OwnedRwLockWriteGuard<T: ?Sized> {
    permits_acquired: usize,
    // ManuallyDrop allows us to destructure into this field without running the destructor.
    lock: ManuallyDrop<Arc<RwLock<T>>>,
    data: *mut T,
    _p: PhantomData<T>,
}

impl<T: ?Sized> OwnedRwLockWriteGuard<T> {
    /// Atomically downgrades a write lock into a read lock without allowing
    /// any writers to take exclusive access of the lock in the meantime.
    ///
    /// **Note:** This won't *necessarily* allow any additional readers to acquire
    /// locks, since [`RwLock`] is fair and it is possible that a writer is next
    /// in line.
    pub fn downgrade(mut self) -> OwnedRwLockReadGuard<T> {
        let lock = unsafe { ManuallyDrop::take(&mut self.lock) };

        let data = self.data;
        let to_release = self.permits_acquired - 1;

        tracing::trace!("rwlock {:p} downgrading to OwnedReadGuard", &self);

        // NB: Forget to avoid drop impl from being called.
        std::mem::forget(self);

        // Release all but one of the permits held by the write guard
        lock.sem.release(to_release);

        OwnedRwLockReadGuard {
            lock: ManuallyDrop::new(lock),
            data,
            _p: PhantomData,
        }
    }
}

impl<T: ?Sized> ops::Deref for OwnedRwLockWriteGuard<T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.data }
    }
}

impl<T: ?Sized> ops::DerefMut for OwnedRwLockWriteGuard<T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.data }
    }
}

impl<T: ?Sized> fmt::Debug for OwnedRwLockWriteGuard<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized> Drop for OwnedRwLockWriteGuard<T> {
    fn drop(&mut self) {
        self.lock.sem.release(self.permits_acquired);
        unsafe { ManuallyDrop::drop(&mut self.lock) };
    }
}
