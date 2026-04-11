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

    // There can only be one upgradable read at a time, so this semaphore
    // is used to ensure that.
    upgradable_read_sem: BatchSemaphore,

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
        RwLock {
            max_readers,
            sem: BatchSemaphore::const_new(max_readers, Fairness::StrictlyFair),
            upgradable_read_sem: BatchSemaphore::const_new(1, Fairness::StrictlyFair),
            inner: UnsafeCell::new(value),
        }
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
        trace!("parking_lot rwlock {:p} read acquiring RwLockReadGuard", self);
        self.sem.acquire_blocking(1).unwrap_or_else(|_| {
            // The semaphore was closed. but, we never explicitly close it, and we have a
            // handle to it through the Arc, which means that this can never happen.
            if !std::thread::panicking() {
                unreachable!()
            }
        });

        trace!("parking_lot rwlock {:p} read acquired RwLockReadGuard", self);

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
        trace!("parking_lot rwlock {:p} try_read acquiring RwlockReadGuard", self);

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

        trace!("parking_lot rwlock {:p} try_read acquired RwLockReadGuard", self);

        Some(RwLockReadGuard {
            sem: &self.sem,
            data: self.inner.get(),
            _p: PhantomData,
        })
    }

    /// Locks this `RwLock` with exclusive write access, blocking the current
    /// thread until it can be acquired.
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        trace!("parking_lot rwlock {:p} write acquiring RwLockWriteGuard", self);
        self.sem.acquire_blocking(self.max_readers).unwrap_or_else(|_| {
            // The semaphore was closed. but, we never explicitly close it, and we have a
            // handle to it through the Arc, which means that this can never happen.
            if !std::thread::panicking() {
                unreachable!()
            }
        });

        trace!("parking_lot rwlock {:p} write acquired RwLockWriteGuard", self);
        RwLockWriteGuard {
            permits_acquired: self.max_readers,
            sem: &self.sem,
            data: self.inner.get(),
            _p: PhantomData,
        }
    }

    /// Attempts to acquire this `RwLock` with exclusive write access.
    ///
    /// If the access couldn't be acquired immediately, returns [`TryLockError`].
    /// Otherwise, an RAII guard is returned which will release write access
    /// when dropped.
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        tracing::trace!("parking_lot rwlock {:p} try_write acquiring RwLockWriteGuard", self);

        match self.sem.try_acquire(self.max_readers) {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => return None,
            Err(TryAcquireError::Closed) => {
                if !std::thread::panicking() {
                    unreachable!()
                }
                return None;
            }
        }

        tracing::trace!("parking_lot rwlock {:p} try_write acquired RwLockWriteGuard", self);

        Some(RwLockWriteGuard {
            permits_acquired: self.max_readers,
            sem: &self.sem,
            data: self.inner.get(),
            _p: PhantomData,
        })
    }

    /// Locks this `RwLock` with upgradable read access, blocking the current
    /// thread until it can be acquired.
    #[inline]
    #[track_caller]
    pub fn upgradable_read(&self) -> RwLockUpgradableReadGuard<'_, T> {
        trace!(
            "parking_lot rwlock {:p} upgradeable_read acquiring upgradable_read_sem",
            self
        );

        self.upgradable_read_sem.acquire_blocking(1).unwrap_or_else(|_| {
            // The semaphore was closed. but, we never explicitly close it, and we have a
            // handle to it through the Arc, which means that this can never happen.
            if !std::thread::panicking() {
                unreachable!()
            }
        });

        trace!(
            "parking_lot rwlock {:p} upgradeable_read acquiring RwLockUpgradableReadGuard",
            self
        );

        let drop_guard = UpgradableRwLockSemWrapper {
            upgradable_read_sem: &self.upgradable_read_sem,
        };

        self.sem.acquire_blocking(1).unwrap_or_else(|_| {
            // The semaphore was closed. but, we never explicitly close it, and we have a
            // handle to it through the Arc, which means that this can never happen.
            if !std::thread::panicking() {
                unreachable!()
            }
        });

        trace!(
            "parking_lot rwlock {:p} upgradeable_read acquired RwLockUpgradableReadGuard",
            self
        );

        RwLockUpgradableReadGuard {
            sem: &self.sem,
            _upgradable_read_sem: drop_guard,
            max_readers: self.max_readers,
            data: self.inner.get(),
            _p: PhantomData,
        }
    }

    /// Attempts to acquire this `RwLock` with upgradable read access.
    ///
    /// If the access could not be granted at this time, then `None` is returned.
    /// Otherwise, an RAII guard is returned which will release the shared access
    /// when it is dropped.
    ///
    /// This function does not block.
    #[inline]
    #[track_caller]
    pub fn try_upgradable_read(&self) -> Option<RwLockUpgradableReadGuard<'_, T>> {
        trace!(
            "parking_lot rwlock {:p} try_upgradeable_read acquiring upgradable_read_sem",
            self
        );

        if let Err(try_acquire_error) = self.upgradable_read_sem.try_acquire(1) {
            trace!(
                "parking_lot rwlock {:p} try_upgradeable_read returned {try_acquire_error:?}, returning None",
                self
            );
            return None;
        }

        let drop_guard = UpgradableRwLockSemWrapper {
            upgradable_read_sem: &self.upgradable_read_sem,
        };

        trace!(
            "parking_lot rwlock {:p} try_upgradeable_read acquiring RwLockUpgradableReadGuard",
            self
        );

        if let Err(try_acquire_error) = self.sem.try_acquire(1) {
            trace!(
                "parking_lot rwlock {:p} try_upgradeable_read returned {try_acquire_error:?}, returning None",
                self
            );
            return None;
        }

        trace!(
            "parking_lot rwlock {:p} try_upgradeable_read acquired RwLockUpgradableReadGuard",
            self
        );

        Some(RwLockUpgradableReadGuard {
            sem: &self.sem,
            _upgradable_read_sem: drop_guard,
            max_readers: self.max_readers,
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

/// An RAII guard for upgradable read access to an `RwLock`.
pub struct RwLockUpgradableReadGuard<'a, T: ?Sized> {
    _upgradable_read_sem: UpgradableRwLockSemWrapper<'a>,
    sem: &'a BatchSemaphore,
    max_readers: usize,
    data: *const T,
    _p: PhantomData<&'a T>,
}

impl<'a, T: ?Sized> RwLockUpgradableReadGuard<'a, T> {
    /// Atomically upgrades an upgradable read lock lock into an exclusive write lock,
    /// blocking the current thread until it can be acquired.
    #[inline]
    pub fn upgrade(s: Self) -> RwLockWriteGuard<'a, T> {
        s.sem.upgrade(1, s.max_readers);

        // When we return, the `UpgradableRwLockSemWrapper` goes out of scope and it's possible for another
        // task to acquire the upgradable read lock.

        RwLockWriteGuard {
            permits_acquired: s.max_readers,
            sem: s.sem,
            data: s.data as *mut T,
            _p: PhantomData,
        }
    }

    /// Atomically downgrades an upgradable read lock lock into a shared read lock
    /// without allowing any writers to take exclusive access of the lock in the
    /// meantime.
    ///
    /// Note that if there are any writers currently waiting to take the lock
    /// then other readers may not be able to acquire the lock even if it was
    /// downgraded.
    #[track_caller]
    pub fn downgrade(s: Self) -> RwLockReadGuard<'a, T> {
        // When we return, the `UpgradableRwLockSemWrapper` goes out of scope and it's possible for another
        // task to acquire the upgradable read lock.

        RwLockReadGuard {
            sem: s.sem,
            data: s.data as *mut T,
            _p: PhantomData,
        }
    }
}

impl<T: ?Sized> std::ops::Deref for RwLockUpgradableReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.data }
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
unsafe impl<T> Send for RwLockReadGuard<'_, T> where T: ?Sized + Send + Sync {}
unsafe impl<T> Sync for RwLockReadGuard<'_, T> where T: ?Sized + Sync {}

// Safety: Stores a raw pointer to `T`, so if `T` is `Sync`, the lock guard over
// `T` is `Send` - but since this is also provides mutable access, we need to
// make sure that `T` is `Send` since its value can be sent across thread
// boundaries.
unsafe impl<T> Send for RwLockWriteGuard<'_, T> where T: ?Sized + Send + Sync {}
unsafe impl<T> Sync for RwLockWriteGuard<'_, T> where T: ?Sized + Sync {}

// SAFETY: The raw pointer is not actually sent across threads
unsafe impl<T> Send for RwLockUpgradableReadGuard<'_, T> where T: ?Sized + Send + Sync {}
// SAFETY: The raw pointer is not actually sent across threads
unsafe impl<T> Sync for RwLockUpgradableReadGuard<'_, T> where T: ?Sized + Sync {}

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

struct UpgradableRwLockSemWrapper<'a> {
    upgradable_read_sem: &'a BatchSemaphore,
}

impl<'a> Drop for UpgradableRwLockSemWrapper<'a> {
    fn drop(&mut self) {
        self.upgradable_read_sem.release(1);
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

        tracing::trace!(
            "parking_lot rwlock {:p} downgrade RwLockWriteGuard to RwLockReadGuard",
            &self
        );

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

#[cfg(test)]
mod tests {
    use super::{RwLock, RwLockUpgradableReadGuard};
    use shuttle::{check_dfs, thread::spawn};
    use std::sync::{Arc, atomic::Ordering};

    #[test]
    #[should_panic = "deadlock"]
    fn mem_forget_write_guard_deadlock() {
        check_dfs(
            move || {
                let rwlock = Arc::new(RwLock::new(()));
                let r1 = rwlock.clone();
                let t1 = spawn(move || {
                    std::mem::forget(r1.write());
                });
                let t2 = spawn(move || {
                    let _g = rwlock.write();
                });
                t1.join().unwrap();
                t2.join().unwrap();
            },
            None,
        );
    }

    // Same as above, but checking that we don't allow multiple upgradable read locks at the same time.
    #[test]
    #[should_panic = "deadlock"]
    fn mem_forget_upgradable_read_guard_deadlock() {
        check_dfs(
            move || {
                let rwlock = Arc::new(RwLock::new(()));
                let r1 = rwlock.clone();
                let t1 = spawn(move || {
                    std::mem::forget(r1.upgradable_read());
                });
                let t2 = spawn(move || {
                    let _g = rwlock.upgradable_read();
                });
                t1.join().unwrap();
                t2.join().unwrap();
            },
            None,
        );
    }

    #[test]
    fn upgrade_sanity() {
        check_dfs(
            move || {
                let rwlock = Arc::new(RwLock::new(0));
                let current_holders = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let current_holders1 = current_holders.clone();
                let r1 = rwlock.clone();
                let r2 = rwlock.clone();
                let t1 = spawn(move || {
                    let guard = r1.upgradable_read();
                    let holders = current_holders.fetch_add(1, Ordering::SeqCst);
                    assert!(holders == 0);
                    assert!(*guard < 2);
                    let mut write = RwLockUpgradableReadGuard::<'_, _>::upgrade(guard);
                    let holders = current_holders.fetch_sub(1, Ordering::SeqCst);
                    assert!(holders == 1);
                    *write += 1;
                });
                let t2 = spawn(move || {
                    let guard = r2.upgradable_read();
                    let holders = current_holders1.fetch_add(1, Ordering::SeqCst);
                    assert!(holders == 0, "{}", format!("{holders}"));
                    assert!(*guard < 2);
                    let mut write = RwLockUpgradableReadGuard::<'_, _>::upgrade(guard);
                    let holders = current_holders1.fetch_sub(1, Ordering::SeqCst);
                    assert!(holders == 1);
                    *write += 1;
                });
                t1.join().unwrap();
                t2.join().unwrap();
                assert!(*rwlock.read() == 2);
            },
            None,
        );
    }

    #[test]
    fn upgradable_read_does_not_block_write() {
        check_dfs(
            move || {
                let rwlock = Arc::new(RwLock::new(0));
                let r1 = rwlock.clone();

                let rg = rwlock.upgradable_read();
                let t2 = spawn(move || {
                    let _g = r1.write();
                });
                let g = RwLockUpgradableReadGuard::upgrade(rg);
                drop(g);
                t2.join().unwrap();
            },
            None,
        );
    }
}
