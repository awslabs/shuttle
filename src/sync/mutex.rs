use crate::future::batch_semaphore::{BatchSemaphore, Fairness, TryAcquireError};
use crate::runtime::task::TaskId;
use crate::current;
use std::cell::RefCell;
use std::fmt::{Debug, Display};
use std::ops::{Deref, DerefMut};
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::{LockResult, PoisonError, TryLockError, TryLockResult};
use tracing::trace;

/// A mutex, the same as [`std::sync::Mutex`].
pub struct Mutex<T: ?Sized> {
    state: RefCell<MutexState>,
    semaphore: BatchSemaphore,
    inner: std::sync::Mutex<T>,
}

/// A mutex guard, the same as [`std::sync::MutexGuard`].
pub struct MutexGuard<'a, T: ?Sized> {
    inner: Option<std::sync::MutexGuard<'a, T>>,
    mutex: &'a Mutex<T>,
}

#[derive(Debug)]
struct MutexState {
    holder: Option<TaskId>,
}

impl<T> Mutex<T> {
    /// Creates a new mutex in an unlocked state ready for use.
    pub const fn new(value: T) -> Self {
        let state = MutexState {
            holder: None,
        };
        Self {
            state: RefCell::new(state),
            semaphore: BatchSemaphore::const_new(1, Fairness::Unfair),
            inner: std::sync::Mutex::new(value),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Acquires a mutex, blocking the current thread until it is able to do so.
    pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        let me = current::me();

        let mut state = self.state.borrow_mut();
        trace!(holder=?state.holder, semaphore=?self.semaphore, "waiting to acquire mutex {:p}", self);
        drop(state);

        // Try to acquire without blocking first. If we succeed, we are done
        // and can move on to acquiring the inner mutex. This is a yield point.
        match self.semaphore.try_acquire(1) {
            Ok(()) => (),
            Err(TryAcquireError::Closed) => unreachable!(),
            Err(_) => {
                // If the lock is already held, then we are blocked. This can
                // happen either because the lock is held by another thread,
                // or because the lock is held by the current thread. For the
                // latter case, we check the state to report a more precise
                // error message.
                state = self.state.borrow_mut();
                if let Some(holder) = state.holder {
                    if holder == me {
                        panic!("deadlock! task {:?} tried to acquire a Mutex it already holds", me);
                    }
                }
                drop(state);

                self.semaphore.acquire_blocking(1).unwrap();
            }
        }

        state = self.state.borrow_mut();
        assert!(state.holder.is_none());
        state.holder = Some(me);
        drop(state);

        trace!(semaphore=?self.semaphore, "acquired mutex {:p}", self);

        // Grab a `MutexGuard` from the inner lock, which we must be able to acquire here
        let result = match self.inner.try_lock() {
            Ok(guard) => Ok(MutexGuard {
                inner: Some(guard),
                mutex: self,
            }),
            Err(TryLockError::Poisoned(guard)) => Err(PoisonError::new(MutexGuard {
                inner: Some(guard.into_inner()),
                mutex: self,
            })),
            Err(TryLockError::WouldBlock) => panic!("mutex state out of sync"),
        };

        result
    }

    /// Attempts to acquire this lock.
    ///
    /// If the lock could not be acquired at this time, then Err is returned. This function does not
    /// block.
    pub fn try_lock(&self) -> TryLockResult<MutexGuard<T>> {
        let me = current::me();

        let mut state = self.state.borrow_mut();
        trace!(holder=?state.holder, semaphore=?self.semaphore, "trying to acquire mutex {:p}", self);
        drop(state);

        // `try_acquire` is a yield point. We need to let other threads in here so they
        // (a) may fail a `try_lock` (in case we acquired), or
        // (b) may release the lock (in case we failed to acquire) so we can succeed in a subsequent `try_lock`.
        self.semaphore.try_acquire(1)
            .map_err(|_| TryLockError::WouldBlock)?;

        state = self.state.borrow_mut();
        state.holder = Some(me);
        drop(state);

        trace!(semaphore=?self.semaphore, "acquired mutex {:p}", self);

        // Grab a `MutexGuard` from the inner lock, which we must be able to acquire here
        let result = match self.inner.try_lock() {
            Ok(guard) => Ok(MutexGuard {
                inner: Some(guard),
                mutex: self,
            }),
            Err(TryLockError::Poisoned(guard)) => Err(TryLockError::Poisoned(PoisonError::new(MutexGuard {
                inner: Some(guard.into_inner()),
                mutex: self,
            }))),
            Err(TryLockError::WouldBlock) => panic!("mutex state out of sync"),
        };

        result
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `Mutex` mutably, no actual locking needs to
    /// take place---the mutable borrow statically guarantees no locks exist.
    #[inline]
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        self.inner.get_mut()
    }

    /// Consumes this mutex, returning the underlying data.
    pub fn into_inner(self) -> LockResult<T>
    where
        T: Sized,
    {
        let state = self.state.borrow();
        assert!(state.holder.is_none());

        // Update the receiver's clock with the Mutex clock
        self.semaphore.try_acquire(1).unwrap();

        self.inner.into_inner()
    }
}

// Safety: Mutex is never actually passed across true threads, only across continuations. The
// Rc<RefCell<_>> type therefore can't be preempted mid-bookkeeping-operation.
// TODO we shouldn't need to do this, but RefCell is not Send, and anything we put within a Mutex
// TODO needs to be Send.
unsafe impl<T: Send + ?Sized> Send for Mutex<T> {}
unsafe impl<T: Send + ?Sized> Sync for Mutex<T> {}

// TODO this is the RefCell biting us again
impl<T: ?Sized> UnwindSafe for Mutex<T> {}
impl<T: ?Sized> RefUnwindSafe for Mutex<T> {}

impl<T: Default + ?Sized> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<T: ?Sized + Debug> Debug for Mutex<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.inner, f)
    }
}

impl<'a, T: ?Sized> MutexGuard<'a, T> {
    /// Release the lock, but return a reference to it so it can be re-acquired later
    pub(super) fn unlock(self) -> &'a Mutex<T> {
        self.mutex
    }
}

impl<'a, T: ?Sized> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        // Release the inner mutex
        self.inner = None;

        let mut state = self.mutex.state.borrow_mut();
        trace!(semaphore=?self.mutex.semaphore, "releasing mutex {:p}", self.mutex);
        state.holder = None;
        drop(state);

        // Release a permit (this is a yield point)
        self.mutex.semaphore.release(1);
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap()
    }
}

impl<T: Debug + ?Sized> Debug for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.inner.as_ref().unwrap(), f)
    }
}

impl<T: Display + ?Sized> Display for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        (**self).fmt(f)
    }
}
