//! An asynchronous `Mutex`-like type.

use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness, TryAcquireError};
use std::cell::UnsafeCell;
use std::error::Error;
use std::fmt::{self, Debug, Display};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::thread;
use tracing::trace;

/// An asynchronous semaphore
pub struct Mutex<T: ?Sized> {
    semaphore: BatchSemaphore,
    inner: UnsafeCell<T>,
}

/// A handle to a held `Mutex`. The guard can be held across any `.await` point
/// as it is [`Send`].
pub struct MutexGuard<'a, T: ?Sized> {
    mutex: &'a Mutex<T>,
}

impl<T: ?Sized + Display> Display for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&**self, f)
    }
}

/// An owned handle to a held `Mutex`.
pub struct OwnedMutexGuard<T: ?Sized> {
    mutex: Arc<Mutex<T>>,
}

/// Error returned from the [`Mutex::try_lock`], `RwLock::try_read` and
/// `RwLock::try_write` functions.
#[derive(Debug)]
pub struct TryLockError(pub(super) ());

impl Display for TryLockError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fmt, "operation would block")
    }
}

impl Error for TryLockError {}

// As long as T: Send, it's fine to send and share Mutex<T> between threads.
// If T was not Send, sending and sharing a Mutex<T> would be bad, since you can
// access T through Mutex<T>.
unsafe impl<T> Send for Mutex<T> where T: ?Sized + Send {}
unsafe impl<T> Sync for Mutex<T> where T: ?Sized + Send {}
unsafe impl<T> Sync for MutexGuard<'_, T> where T: ?Sized + Send + Sync {}
unsafe impl<T> Sync for OwnedMutexGuard<T> where T: ?Sized + Send + Sync {}

impl<T: ?Sized> Mutex<T> {
    /// Creates a new lock in an unlocked state ready for use.
    pub fn new(t: T) -> Self
    where
        T: Sized,
    {
        Self {
            semaphore: BatchSemaphore::new(1, Fairness::StrictlyFair),
            inner: UnsafeCell::new(t),
        }
    }

    async fn acquire(&self) {
        trace!("acquiring lock {:p}", self);
        self.semaphore.acquire(1).await.unwrap_or_else(|_| {
            // The semaphore was closed. but, we never explicitly close it, and
            // we own it exclusively, which means that this can never happen.
            if !thread::panicking() {
                unreachable!()
            }
        });
        trace!("acquired lock {:p}", self);
    }

    /// Locks this mutex, causing the current task to yield until the lock has
    /// been acquired.  When the lock has been acquired, function returns a
    /// [`MutexGuard`].    
    pub async fn lock(&self) -> MutexGuard<'_, T> {
        self.acquire().await;

        MutexGuard { mutex: self }
    }

    /// Blockingly locks this `Mutex`. When the lock has been acquired, function returns a
    /// [`MutexGuard`].
    ///
    /// This method is intended for use cases where you
    /// need to use this mutex in asynchronous code as well as in synchronous code.    
    pub fn blocking_lock(&self) -> MutexGuard<'_, T> {
        shuttle::future::block_on(self.lock())
    }

    /// Locks this mutex, causing the current task to yield until the lock has
    /// been acquired. When the lock has been acquired, this returns an
    /// [`OwnedMutexGuard`].
    pub async fn lock_owned(self: Arc<Self>) -> OwnedMutexGuard<T> {
        self.acquire().await;

        OwnedMutexGuard { mutex: self }
    }

    fn try_acquire(&self) -> Result<(), TryAcquireError> {
        self.semaphore.try_acquire(1)
    }

    /// Attempts to acquire the lock, and returns [`TryLockError`] if the
    /// lock is currently held somewhere else.
    pub fn try_lock(&self) -> Result<MutexGuard<'_, T>, TryLockError> {
        match self.try_acquire() {
            Ok(()) => Ok(MutexGuard { mutex: self }),
            Err(_) => Err(TryLockError(())),
        }
    }

    /// Attempts to acquire the lock, and returns [`TryLockError`] if the lock
    /// is currently held somewhere else.
    pub fn try_lock_owned(self: Arc<Self>) -> Result<OwnedMutexGuard<T>, TryLockError> {
        match self.try_acquire() {
            Ok(()) => Ok(OwnedMutexGuard { mutex: self }),
            Err(_) => Err(TryLockError(())),
        }
    }

    /// Consumes the mutex, returning the underlying data.
    pub fn into_inner(self) -> T
    where
        T: Sized,
    {
        self.inner.into_inner()
    }
}

impl<T: ?Sized + Debug> Debug for Mutex<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SAFETY: Shuttle is running single-threaded, only we are able to access `inner` at the time of this call.
        Debug::fmt(&unsafe { &*self.inner.get() }, f)
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mutex.inner.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mutex.inner.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        trace!("releasing lock {:p}", self);
        self.mutex.semaphore.release(1);
    }
}

impl<T: ?Sized + Debug> Debug for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.mutex, f)
    }
}

impl<T: ?Sized> Drop for OwnedMutexGuard<T> {
    fn drop(&mut self) {
        trace!("releasing owned lock {:p}", self);
        self.mutex.semaphore.release(1);
    }
}

impl<T: ?Sized> Deref for OwnedMutexGuard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mutex.inner.get() }
    }
}

impl<T: ?Sized> DerefMut for OwnedMutexGuard<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mutex.inner.get() }
    }
}

impl<T: ?Sized + Debug> Debug for OwnedMutexGuard<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.mutex, f)
    }
}

impl<T> From<T> for Mutex<T> {
    fn from(s: T) -> Self {
        Self::new(s)
    }
}

impl<T> Default for Mutex<T>
where
    T: Default,
{
    fn default() -> Self {
        Self::new(T::default())
    }
}
