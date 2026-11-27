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
        trace!("rwlock {:p} read", self);
        let inner = self.sem.acquire(1);
        inner.await.unwrap_or_else(|_| {
            // The semaphore was closed. We never close it, but `BatchSemaphore` closes itself when
            // a permit is released while `ExecutionState::should_stop()` is true (see
            // `BatchSemaphore::release`), for example after the execution stopped or failed, or
            // outside an execution. If `std::thread::panicking()` is true then, this closure
            // returns, and this function makes a guard right below without the permits. In Shuttle
            // `std::thread::panicking()` is true while any task on this OS thread is unwinding, not
            // only this one. The guards' SAFETY comments rely on the permits, so such a guard
            // breaks them.
            if !std::thread::panicking() {
                unreachable!()
            }
        });

        trace!("rwlock {:p} read acquired ReadGuard", self);
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
        trace!("rwlock {:p} read_owned", self);
        let inner = self.sem.acquire(1);
        inner.await.unwrap_or_else(|_| {
            // The semaphore was closed. We never close it, but `BatchSemaphore` closes itself when
            // a permit is released while `ExecutionState::should_stop()` is true (see
            // `BatchSemaphore::release`), for example after the execution stopped or failed, or
            // outside an execution. If `std::thread::panicking()` is true then, this closure
            // returns, and this function makes a guard right below without the permits. In Shuttle
            // `std::thread::panicking()` is true while any task on this OS thread is unwinding, not
            // only this one. The guards' SAFETY comments rely on the permits, so such a guard
            // breaks them.
            if !std::thread::panicking() {
                unreachable!()
            }
        });

        trace!("rwlock {:p} read_owned acquired OwnedReadGuard", self);
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
        tracing::trace!("rwlock {:p} try_read", self);
        match self.sem.try_acquire(1) {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => return Err(TryLockError(())),
            Err(TryAcquireError::Closed) => {
                if !std::thread::panicking() {
                    unreachable!()
                }
                return Err(TryLockError(()));
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
        trace!("rwlock {:p} try_read_owned", self);
        match self.sem.try_acquire(1) {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => return Err(TryLockError(())),
            Err(TryAcquireError::Closed) => {
                if !std::thread::panicking() {
                    unreachable!()
                }
                return Err(TryLockError(()));
            }
        }

        trace!("rwlock {:p} try_read_owned acquired OwnedReadGuard", self);
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
        trace!("rwlock {:p} write", self);
        self.sem.acquire(self.max_readers).await.unwrap_or_else(|_| {
            // The semaphore was closed. We never close it, but `BatchSemaphore` closes itself when
            // a permit is released while `ExecutionState::should_stop()` is true (see
            // `BatchSemaphore::release`), for example after the execution stopped or failed, or
            // outside an execution. If `std::thread::panicking()` is true then, this closure
            // returns, and this function makes a guard right below without the permits. In Shuttle
            // `std::thread::panicking()` is true while any task on this OS thread is unwinding, not
            // only this one. The guards' SAFETY comments rely on the permits, so such a guard
            // breaks them.
            if !std::thread::panicking() {
                unreachable!()
            }
        });

        trace!("rwlock {:p} write acquired WriteGuard", self);
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
        tracing::trace!("rwlock {:p} write_owned", self);
        self.sem.acquire(self.max_readers).await.unwrap_or_else(|_| {
            // The semaphore was closed. We never close it, but `BatchSemaphore` closes itself when
            // a permit is released while `ExecutionState::should_stop()` is true (see
            // `BatchSemaphore::release`), for example after the execution stopped or failed, or
            // outside an execution. If `std::thread::panicking()` is true then, this closure
            // returns, and this function makes a guard right below without the permits. In Shuttle
            // `std::thread::panicking()` is true while any task on this OS thread is unwinding, not
            // only this one. The guards' SAFETY comments rely on the permits, so such a guard
            // breaks them.
            if !std::thread::panicking() {
                unreachable!()
            }
        });

        tracing::trace!("rwlock {:p} write_owned acquired OwnedWriteGuard", self);
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
        tracing::trace!("rwlock {:p} try_write", self);
        match self.sem.try_acquire(self.max_readers) {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => return Err(TryLockError(())),
            Err(TryAcquireError::Closed) => {
                if !std::thread::panicking() {
                    unreachable!()
                }
                return Err(TryLockError(()));
            }
        }

        tracing::trace!("rwlock {:p} try_write acquired WriteGuard", self);
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
        tracing::trace!("rwlock {:p} try_write_owned", self);
        match self.sem.try_acquire(self.max_readers) {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => return Err(TryLockError(())),
            Err(TryAcquireError::Closed) => {
                if !std::thread::panicking() {
                    unreachable!()
                }
                return Err(TryLockError(()));
            }
        }

        tracing::trace!("rwlock {:p} try_write_owned acquired OwnedWriteGuard", self);
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
        self.inner.get_mut()
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

// The guards' impls below are the same as tokio's. The guards hold raw pointers, so the compiler
// would make none of them `Send` or `Sync` by itself.
//
// Every guard holds either all of `sem`'s permits (a write guard) or one of them (a read guard),
// and `sem` starts with `max_readers` permits. So while a write guard exists, no other guard does,
// and while a read guard exists, no write guard does. Each impl below relies on this rule. The
// `unwrap_or_else` in `read`, `read_owned`, `write` and `write_owned` breaks it, see the comment
// there.
//
// A thread that shares the lock, or gets a guard, uses the lock's `BatchSemaphore`: to lock, and
// to release the permits when it drops a guard. `BatchSemaphore` is `Sync` by an `unsafe impl` in
// shuttle-engine (see its SAFETY comment). That impl does not hold when two OS threads use one
// semaphore at the same time, for example two tests that run at the same time and share one lock
// through a `static`. So the impls below that say they rely on it do not hold then either.

// `RwLock<T>` is `Send` when `T` is, without an `unsafe impl` of its own, because each of its
// fields is `Send` then. This checks that it stays so, for every `T: Send`.
const _: () = {
    const fn assert_send<T: Send + ?Sized>() {}
    #[allow(dead_code)]
    const fn rwlock_is_send<T: Send + ?Sized>() {
        assert_send::<RwLock<T>>();
    }
};

// SAFETY:
// In order for `RwLock<T>` to be `Sync`, we need to ensure:
// * threads that share a `&RwLock<T>` can share the `T`.
// * the `T` may move to another thread.
// * no two OS threads use the lock's `BatchSemaphore` at the same time.
//
// Many threads can hold read guards at the same time, and each gets a `&T`. So `T` must be `Sync`.
// A thread that holds a write guard has the only guard (see above), and can move the `T` out
// through `&mut T`. Another thread can then drop it. So `T` must be `Send`.
// The third relies on the `Sync` impl of `BatchSemaphore`, and does not always hold (see above).
unsafe impl<T> Sync for RwLock<T> where T: ?Sized + Send + Sync {}

// SAFETY:
// In order for `RwLockReadGuard<'_, T>` to be `Send`, we need to ensure:
// * the thread that gets the guard can share the `T` with the threads that hold other read guards.
// * that thread may use the lock's `BatchSemaphore`.
//
// The guard gives only a `&T`, and other read guards can exist on other threads at the same time.
// So `T: Sync` is enough, which the bound requires.
// Dropping the guard releases a permit through the `&BatchSemaphore` it holds. This relies on the
// `Sync` impl of `BatchSemaphore`, and does not always hold (see above).
unsafe impl<T> Send for RwLockReadGuard<'_, T> where T: ?Sized + Sync {}

// SAFETY:
// In order for `RwLockReadGuard<'_, T>` to be `Sync`, we need to ensure:
// * threads that share a `&RwLockReadGuard<'_, T>` can share the `T`.
//
// A shared read guard gives each thread only a `&T`, so `T: Sync` is enough. The bound also
// requires `T: Send`, which is stricter than needed. No `&self` method of the guard uses the
// semaphore.
unsafe impl<T> Sync for RwLockReadGuard<'_, T> where T: ?Sized + Send + Sync {}

// SAFETY:
// In order for `OwnedRwLockReadGuard<T, U>` to be `Send`, we need to ensure:
// * the thread that gets the guard can share the `U` with the threads that hold other read guards.
// * the `Arc<RwLock<T>>` in the guard may move to another thread.
//
// The guard gives only a `&U`, so `U: Sync` is enough for the first property. The guard holds an
// `Arc<RwLock<T>>`, which is `Send` only if `RwLock<T>` is `Send` and `Sync`, that is if `T` is
// `Send` and `Sync`. Unlike `RwLockReadGuard`, the guard can drop the last `Arc`, and with it the
// `T`, on the other thread. The bounds require all of this. This file makes these guards only with
// `U` the same as `T`.
// `RwLock<T>: Sync` relies on the `Sync` impl of `BatchSemaphore`, and does not always hold (see
// above).
unsafe impl<T, U> Send for OwnedRwLockReadGuard<T, U>
where
    T: ?Sized + Send + Sync,
    U: ?Sized + Sync,
{
}

// SAFETY:
// In order for `OwnedRwLockReadGuard<T, U>` to be `Sync`, we need to ensure:
// * threads that share a `&OwnedRwLockReadGuard<T, U>` can share the `U`.
// * those threads can share the `Arc<RwLock<T>>` in the guard.
//
// A shared guard gives each thread a `&U`, so `U` must be `Sync`. `rwlock` gives each thread a
// `&Arc<RwLock<T>>`, which is `Sync` only if `T` is `Send` and `Sync`. The bounds require this, and
// `U: Send` as well, which is stricter than needed.
// `RwLock<T>: Sync` relies on the `Sync` impl of `BatchSemaphore`, and does not always hold (see
// above).
unsafe impl<T, U> Sync for OwnedRwLockReadGuard<T, U>
where
    T: ?Sized + Send + Sync,
    U: ?Sized + Send + Sync,
{
}

// SAFETY:
// In order for `RwLockWriteGuard<'_, T>` to be `Sync`, we need to ensure:
// * threads that share a `&RwLockWriteGuard<'_, T>` can share the `T`.
//
// A shared write guard gives each thread only a `&T`, because `deref_mut` needs `&mut self`. So
// `T: Sync` is enough. The bound also requires `T: Send`, which is stricter than needed. No `&self`
// method of the guard uses the semaphore.
unsafe impl<T> Sync for RwLockWriteGuard<'_, T> where T: ?Sized + Send + Sync {}

// SAFETY:
// In order for `OwnedRwLockWriteGuard<T>` to be `Sync`, we need to ensure:
// * threads that share a `&OwnedRwLockWriteGuard<T>` can share the `T`.
// * those threads can share the `Arc<RwLock<T>>` in the guard.
//
// A shared write guard gives each thread only a `&T`, so `T` must be `Sync`. `rwlock` gives each
// thread a `&Arc<RwLock<T>>`, which is `Sync` only if `T` is `Send` and `Sync`. The bound requires
// both. `RwLock<T>: Sync` relies on the `Sync` impl of `BatchSemaphore`, and does not always hold
// (see above).
unsafe impl<T> Sync for OwnedRwLockWriteGuard<T> where T: ?Sized + Send + Sync {}

// SAFETY:
// In order for `RwLockWriteGuard<'_, T>` to be `Send`, we need to ensure:
// * the `T` may move to the thread that gets the guard.
// * that thread can share the `T` with other threads.
// * that thread may use the lock's `BatchSemaphore`.
//
// That thread gets a `&mut T`, and can move the `T` out or put another `T` in. So `T` must be
// `Send`.
// `T` must also be `Sync`. `downgrade` makes the guard a read guard on the thread that got it, and
// other threads can then take read guards too, so they share the `&T`. The bound requires both.
// Dropping the guard releases the permits through the `&BatchSemaphore` it holds. This relies on
// the `Sync` impl of `BatchSemaphore`, and does not always hold (see above).
unsafe impl<T> Send for RwLockWriteGuard<'_, T> where T: ?Sized + Send + Sync {}

// SAFETY:
// In order for `OwnedRwLockWriteGuard<T>` to be `Send`, we need to ensure:
// * the `T` may move to the thread that gets the guard.
// * that thread can share the `T` with other threads.
// * the `Arc<RwLock<T>>` in the guard may move to that thread.
//
// That thread gets a `&mut T`, so `T` must be `Send`. `downgrade` makes the guard a read guard on
// that thread, and other threads can then take read guards too, so `T` must also be `Sync`. The
// `Arc<RwLock<T>>` is `Send` only if `T` is `Send` and `Sync`. The bound requires both.
// `RwLock<T>: Sync` relies on the `Sync` impl of `BatchSemaphore`, and does not always hold (see
// above).
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
        // SAFETY:
        // In order for the dereference to be safe, we need to ensure:
        // * `data` is valid for as long as the returned reference.
        // * no `&mut T` to the value is live while the returned reference is.
        //
        // `data` points into the lock, which the guard borrows for `'a`. The returned reference
        // borrows the guard, so it can't outlive the lock.
        // The guard holds a read permit, so no write guard exists (see the rule above the `Send`
        // and `Sync` impls). Only a write guard makes a `&mut T`.
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

impl<T: ?Sized, U: ?Sized> OwnedRwLockReadGuard<T, U> {
    /// Returns a reference to the original `Arc<RwLock>`.
    pub fn rwlock(this: &Self) -> &Arc<RwLock<T>> {
        &this.lock
    }
}

impl<T: ?Sized, U: ?Sized> ops::Deref for OwnedRwLockReadGuard<T, U> {
    type Target = U;

    fn deref(&self) -> &U {
        // SAFETY:
        // In order for the dereference to be safe, we need to ensure:
        // * `data` is valid for as long as the returned reference.
        // * no `&mut` to the value is live while the returned reference is.
        //
        // `data` points into the lock that `self.lock`, an `Arc`, keeps alive. The guard drops that
        // `Arc` only in its `drop`, and the returned reference borrows the guard.
        // The guard holds a read permit, so no write guard exists (see the rule above the `Send`
        // and `Sync` impls). Only a write guard makes a `&mut`.
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

        // SAFETY:
        // In order for the call to be safe, we need to ensure:
        // * `self.lock` is not used after this call, and was not dropped before it.
        //
        // This is the last use of `self.lock` in `drop`, and the guard's other fields have no drop
        // glue. Nothing else drops `self.lock`: this file takes no `Arc` out of a read guard. If
        // `release` panics, this call does not run, and the `Arc` leaks.
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
        // SAFETY:
        // In order for the dereference to be safe, we need to ensure:
        // * `data` is valid for as long as the returned reference.
        // * no `&mut T` to the value is live while the returned reference is.
        //
        // `data` points into the lock, which the guard borrows for `'a`. The returned reference
        // borrows the guard, so it can't outlive the lock.
        // The guard holds all of the lock's permits, so no other guard exists (see the rule above
        // the `Send` and `Sync` impls). This guard makes a `&mut T` only in `deref_mut`, which
        // takes `&mut self`, so that can't happen while the returned reference is live.
        unsafe { &*self.data }
    }
}

impl<T: ?Sized> ops::DerefMut for RwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY:
        // In order for the dereference to be safe, we need to ensure:
        // * `data` is valid for as long as the returned reference.
        // * no other reference to the value is live while the returned reference is.
        //
        // `data` is valid for the reason that `deref` gives.
        // No other guard exists, for the reason that `deref` gives. The returned reference borrows
        // this guard mutably, so this guard makes no other reference while it is live.
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
        // SAFETY:
        // In order for the call to be safe, we need to ensure:
        // * `self.lock` is not used or dropped after this call.
        //
        // The code below reads only `self.data` and `self.permits_acquired`, and formats the
        // address of `self`. Then `mem::forget(self)` keeps the guard's `drop` from using
        // `self.lock` again.
        // This does not hold if the `trace!` below panics, which a `tracing` subscriber can do. The
        // unwinding drops `lock`, which frees the lock if the guard held the last `Arc`. Then it
        // runs the guard's `drop`, which calls `release` through `self.lock`, a use after free in
        // that case, and drops the same `Arc` a second time. tokio forgets the guard before
        // anything that can panic.
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

    /// Returns a reference to the original `Arc<RwLock>`.
    pub fn rwlock(this: &Self) -> &Arc<RwLock<T>> {
        &this.lock
    }
}

impl<T: ?Sized> ops::Deref for OwnedRwLockWriteGuard<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY:
        // In order for the dereference to be safe, we need to ensure:
        // * `data` is valid for as long as the returned reference.
        // * no `&mut T` to the value is live while the returned reference is.
        //
        // `data` points into the lock that `self.lock`, an `Arc`, keeps alive. The guard drops that
        // `Arc` only in its `drop` or `downgrade`, which take the guard, and the returned
        // reference borrows the guard.
        // No other guard exists, and this guard makes no `&mut T` while the returned reference is
        // live, for the reasons that `RwLockWriteGuard::deref` gives.
        unsafe { &*self.data }
    }
}

impl<T: ?Sized> ops::DerefMut for OwnedRwLockWriteGuard<T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY:
        // In order for the dereference to be safe, we need to ensure:
        // * `data` is valid for as long as the returned reference.
        // * no other reference to the value is live while the returned reference is.
        //
        // `data` is valid for the reason that `deref` gives.
        // No other guard exists, for the reason that `RwLockWriteGuard::deref` gives. The returned
        // reference borrows this guard mutably, so this guard makes no other reference while it is
        // live.
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

        // SAFETY:
        // In order for the call to be safe, we need to ensure:
        // * `self.lock` is not used after this call, and was not dropped before it.
        //
        // This is the last use of `self.lock` in `drop`, and the guard's other fields have no drop
        // glue. The only other place that takes `self.lock` is `downgrade`, which forgets the guard
        // so that this `drop` does not run, except on the panic that its SAFETY comment describes.
        // If `release` panics, this call does not run, and the `Arc` leaks.
        unsafe { ManuallyDrop::drop(&mut self.lock) };
    }
}
