//! An asynchronous `Mutex`-like type.

use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness, TryAcquireError};
use std::cell::UnsafeCell;
use std::error::Error;
use std::fmt::{self, Debug, Display};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::thread;
use tracing::trace;

/// An asynchronous mutex
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

// `Mutex<T>` is `Send` when `T` is, without an `unsafe impl` of its own, because each of its fields
// is `Send` then. This checks that it stays so, for every `T: Send`.
const _: () = {
    const fn assert_send<T: Send + ?Sized>() {}
    #[allow(dead_code)]
    const fn mutex_is_send<T: Send + ?Sized>() {
        assert_send::<Mutex<T>>();
    }
};

// SAFETY:
// In order for `Mutex<T>` to be `Sync`, we need to ensure:
// * threads that share a `&Mutex<T>` use the `T` one at a time.
// * the `T` may move to another thread.
// * no two OS threads use the mutex's `BatchSemaphore` at the same time.
//
// `semaphore` has one permit. A guard is made only after its permit is taken, and the guard gives
// the permit back when it is dropped. Only a guard gives access to the `T`. So the threads use the
// `T` one at a time, and `T: Sync` is not needed. Two paths break this rule. `Debug for Mutex`
// reads the `T` without the permit, and `lock` and `lock_owned` make a guard without the permit
// if the semaphore closed itself. See the comments in `Debug for Mutex` and `acquire`.
// A thread that holds a guard can move the `T` out through `&mut T`, and another thread can then
// drop it. So `T` must be `Send`, which the bound requires.
// The third relies on the `Sync` impl of `BatchSemaphore` (see its SAFETY comment). That impl does
// not hold when two OS threads use one semaphore at the same time, for example two tests that run
// at the same time and share one mutex through a `static`. So this impl does not hold then either.
unsafe impl<T> Sync for Mutex<T> where T: ?Sized + Send {}

// SAFETY:
// In order for `MutexGuard<'_, T>` to be `Sync`, we need to ensure:
// * threads that share a `&MutexGuard<'_, T>` can share the `T`.
//
// Through a shared guard, each thread gets only a `&T` (from `Deref`), and so `T: Sync` is enough.
// The bound also requires `T: Send`, which is stricter than needed. Without this impl, the compiler
// would make the guard `Sync` whenever `T: Send`, because `&Mutex<T>` is then `Sync`. That would
// let two threads call `Cell::set` through one guard at the same time.
unsafe impl<T> Sync for MutexGuard<'_, T> where T: ?Sized + Send + Sync {}

// SAFETY:
// In order for `OwnedMutexGuard<T>` to be `Sync`, we need to ensure:
// * threads that share a `&OwnedMutexGuard<T>` can share the `T`.
//
// This is the same as for `MutexGuard` above. Without this impl, the compiler would make the guard
// `Sync` whenever `T: Send`, because `Arc<Mutex<T>>` is then `Sync`.
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
            // The semaphore was closed. We never close it, but `BatchSemaphore` closes itself
            // when a permit is released while `ExecutionState::should_stop()` is true (see
            // `BatchSemaphore::release`), for example after the execution stopped or failed, or
            // outside an execution. If `thread::panicking()` is true then, this returns, and `lock`
            // or `lock_owned` makes a guard without the permit. In Shuttle `thread::panicking()` is
            // true while any task on this OS thread is unwinding, not only this one. The guards'
            // SAFETY comments rely on the permit, so such a guard breaks them.
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
        // SAFETY:
        // In order for the dereference to be safe, we need to ensure:
        // * no `&mut T` to the value is live while the `&T` is.
        //
        // This does not hold. The read takes no permit. The task that holds the guard, or another
        // task while that task is switched out, can keep a `&mut T` from `deref_mut` live, and the
        // `&T` here aliases it. For example, a task can format the mutex between two uses of a
        // `&mut T` from its own guard, with no `.await` between them. Another task can format it
        // while the holder waits at any scheduling point, in async or in blocking code. The read
        // can also see the value in the middle of a change. tokio's `Debug for Mutex` takes the
        // lock with `try_lock`, and prints `<locked>` when it can't.
        let value = unsafe { &*self.inner.get() };
        Debug::fmt(&value, f)
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY:
        // In order for the dereference to be safe, we need to ensure:
        // * the pointer is valid for as long as the returned reference.
        // * no `&mut T` to the value is live while the returned reference is.
        //
        // The pointer points into `self.mutex`, which the guard borrows for `'a`. The returned
        // reference borrows the guard, so it can't outlive the mutex.
        // The guard holds the only permit of the mutex's semaphore, so no other guard exists (see
        // `Sync for Mutex<T>`, which also names the two paths that break this). This guard makes a
        // `&mut T` only in `deref_mut`, which takes `&mut self`, so that can't happen while the
        // returned reference is live.
        unsafe { &*self.mutex.inner.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY:
        // In order for the dereference to be safe, we need to ensure:
        // * the pointer is valid for as long as the returned reference.
        // * no other reference to the value is live while the returned reference is.
        //
        // The pointer is valid for the reason that `deref` gives.
        // No other guard exists, for the reason that `deref` gives. The returned reference borrows
        // this guard mutably, so this guard makes no other reference while it is live.
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
        // SAFETY:
        // In order for the dereference to be safe, we need to ensure:
        // * the pointer is valid for as long as the returned reference.
        // * no `&mut T` to the value is live while the returned reference is.
        //
        // The pointer points into the mutex that `self.mutex`, an `Arc`, keeps alive. The returned
        // reference borrows the guard, so it can't outlive that `Arc`.
        // No other guard exists, and this guard makes no `&mut T` while the returned reference is
        // live, for the reasons that `MutexGuard::deref` gives.
        unsafe { &*self.mutex.inner.get() }
    }
}

impl<T: ?Sized> DerefMut for OwnedMutexGuard<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY:
        // In order for the dereference to be safe, we need to ensure:
        // * the pointer is valid for as long as the returned reference.
        // * no other reference to the value is live while the returned reference is.
        //
        // The pointer is valid for the reason that `deref` gives.
        // No other guard exists, for the reason that `MutexGuard::deref` gives. The returned
        // reference borrows this guard mutably, so this guard makes no other reference while it is
        // live.
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
