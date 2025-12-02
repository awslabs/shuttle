//! Counting semaphore performing asynchronous permit acquisition.

use shuttle::future::batch_semaphore::{AcquireError, BatchSemaphore, Fairness, TryAcquireError};
use std::sync::Arc;

/// Counting semaphore performing asynchronous permit acquisition.
#[derive(Debug)]
pub struct Semaphore {
    sem: BatchSemaphore,
}

/// A permit from the semaphore.
#[must_use]
#[derive(Debug)]
pub struct SemaphorePermit<'a> {
    sem: &'a Semaphore,
    permits: u32,
}

/// An owned permit from the semaphore.
#[must_use]
#[derive(Debug)]
pub struct OwnedSemaphorePermit {
    sem: Arc<Semaphore>,
    permits: u32,
}

impl Semaphore {
    /// The maximum number of permits which a semaphore can hold. It is `usize::MAX >> 3`.
    ///
    /// Exceeding this limit typically results in a panic.
    pub const MAX_PERMITS: usize = usize::MAX >> 3;

    /// Creates a new semaphore with the initial number of permits.
    pub fn new(num_permits: usize) -> Self {
        let sem = BatchSemaphore::new(num_permits, Fairness::StrictlyFair);
        Self { sem }
    }

    /// Creates a new semaphore with the initial number of permits.
    pub const fn const_new(num_permits: usize) -> Self {
        let sem = BatchSemaphore::const_new(num_permits, Fairness::StrictlyFair);
        Self { sem }
    }

    /// Returns the current number of available permits.
    pub fn available_permits(&self) -> usize {
        self.sem.available_permits()
    }

    /// Adds `n` new permits to the semaphore.
    /// The maximum number of permits is `usize::MAX >> 3`, and this function will panic if the limit is exceeded.
    pub fn add_permits(&self, n: usize) {
        self.sem.release(n);
    }

    /// Acquires a permit from the semaphore.
    ///
    /// If the semaphore has been closed, this returns an [`AcquireError`].
    /// Otherwise, this returns a [`SemaphorePermit`] representing the
    /// acquired permit.
    pub async fn acquire(&self) -> Result<SemaphorePermit<'_>, AcquireError> {
        self.acquire_many(1).await
    }

    /// Acquires `n` permits from the semaphore.
    ///
    /// If the semaphore has been closed, this returns an [`AcquireError`].
    /// Otherwise, this returns a [`SemaphorePermit`] representing the
    /// acquired permits.
    pub async fn acquire_many(&self, permits: u32) -> Result<SemaphorePermit<'_>, AcquireError> {
        self.sem.acquire(permits as usize).await?;
        Ok(SemaphorePermit { sem: self, permits })
    }

    /// Tries to acquire a permit from the semaphore.
    ///
    /// If the semaphore has been closed, this returns a [`TryAcquireError::Closed`]
    /// and a [`TryAcquireError::NoPermits`] if there are no permits left. Otherwise,
    /// this returns a [`SemaphorePermit`] representing the acquired permits.
    pub fn try_acquire(&self) -> Result<SemaphorePermit<'_>, TryAcquireError> {
        self.try_acquire_many(1)
    }

    /// Tries to acquire `n` permits from the semaphore.
    ///
    /// If the semaphore has been closed, this returns a [`TryAcquireError::Closed`]
    /// and a [`TryAcquireError::NoPermits`] if there are not enough permits left.
    /// Otherwise, this returns a [`SemaphorePermit`] representing the acquired permits.
    pub fn try_acquire_many(&self, permits: u32) -> Result<SemaphorePermit<'_>, TryAcquireError> {
        match self.sem.try_acquire(permits as usize) {
            Ok(()) => Ok(SemaphorePermit { sem: self, permits }),
            Err(e) => Err(e),
        }
    }

    /// Acquires an owned permit from the semaphore.
    ///
    /// The semaphore must be wrapped in an [`Arc`] to call this method.
    /// If the semaphore has been closed, this returns an [`AcquireError`].
    /// Otherwise, this returns a [`OwnedSemaphorePermit`] representing the
    /// acquired permit.
    pub async fn acquire_owned(self: Arc<Self>) -> Result<OwnedSemaphorePermit, AcquireError> {
        self.acquire_many_owned(1).await
    }

    /// Acquires `n` owned permits from the semaphore.
    ///
    /// The semaphore must be wrapped in an [`Arc`] to call this method.
    /// If the semaphore has been closed, this returns an [`AcquireError`].
    /// Otherwise, this returns a [`OwnedSemaphorePermit`] representing the
    /// acquired permit.
    pub async fn acquire_many_owned(self: Arc<Self>, permits: u32) -> Result<OwnedSemaphorePermit, AcquireError> {
        self.sem.acquire(permits as usize).await?;
        Ok(OwnedSemaphorePermit { sem: self, permits })
    }

    /// Tries to acquire an owned permit from the semaphore.
    ///
    /// The semaphore must be wrapped in an [`Arc`] to call this method. If
    /// the semaphore has been closed, this returns a [`TryAcquireError::Closed`]
    /// and a [`TryAcquireError::NoPermits`] if there are no permits left.
    /// Otherwise, this returns a [`OwnedSemaphorePermit`] representing the
    /// acquired permit.
    pub fn try_acquire_owned(self: Arc<Self>) -> Result<OwnedSemaphorePermit, TryAcquireError> {
        self.try_acquire_many_owned(1)
    }

    /// Tries to acquire `n` owned permits from the semaphore.
    ///
    /// The semaphore must be wrapped in an [`Arc`] to call this method. If
    /// the semaphore has been closed, this returns a [`TryAcquireError::Closed`]
    /// and a [`TryAcquireError::NoPermits`] if there are no permits left.
    /// Otherwise, this returns a [`OwnedSemaphorePermit`] representing the
    /// acquired permit.
    pub fn try_acquire_many_owned(self: Arc<Self>, permits: u32) -> Result<OwnedSemaphorePermit, TryAcquireError> {
        match self.sem.try_acquire(permits as usize) {
            Ok(()) => Ok(OwnedSemaphorePermit { sem: self, permits }),
            Err(e) => Err(e),
        }
    }

    /// Closes the semaphore.
    ///
    /// This prevents the semaphore from issuing new permits and notifies all pending waiters.
    pub fn close(&self) {
        self.sem.close();
    }

    /// Returns true if the semaphore is closed
    pub fn is_closed(&self) -> bool {
        self.sem.is_closed()
    }
}

impl SemaphorePermit<'_> {
    /// Forgets the permit **without** releasing it back to the semaphore.
    /// This can be used to reduce the amount of permits available from a
    /// semaphore.
    pub fn forget(mut self) {
        self.permits = 0;
    }

    /// Merge two [`SemaphorePermit`] instances together, consuming `other`
    /// without releasing the permits it holds.
    ///
    /// Permits held by both `self` and `other` are released when `self` drops.
    ///
    /// # Panics
    ///
    /// This function panics if permits from different [`Semaphore`] instances
    /// are merged.
    #[track_caller]
    pub fn merge(&mut self, mut other: Self) {
        assert!(
            std::ptr::eq(self.sem, other.sem),
            "merging permits from different semaphore instances"
        );
        self.permits += other.permits;
        other.permits = 0;
    }

    /// Splits `n` permits from `self` and returns a new [`SemaphorePermit`] instance that holds `n` permits.
    ///
    /// If there are insufficient permits and it's not possible to reduce by `n`, returns `None`.
    pub fn split(&mut self, n: usize) -> Option<Self> {
        let n = u32::try_from(n).ok()?;

        if n > self.permits {
            return None;
        }

        self.permits -= n;

        Some(Self {
            sem: self.sem,
            permits: n,
        })
    }

    /// Returns the number of permits held by `self`.
    pub fn num_permits(&self) -> usize {
        self.permits as usize
    }
}

impl OwnedSemaphorePermit {
    /// Forgets the permit **without** releasing it back to the semaphore.
    /// This can be used to reduce the amount of permits available from a
    /// semaphore.
    pub fn forget(mut self) {
        self.permits = 0;
    }

    /// Merge two [`OwnedSemaphorePermit`] instances together, consuming `other`
    /// without releasing the permits it holds.
    ///
    /// Permits held by both `self` and `other` are released when `self` drops.
    #[track_caller]
    pub fn merge(&mut self, mut other: Self) {
        assert!(
            Arc::ptr_eq(&self.sem, &other.sem),
            "merging permits from different semaphore instances"
        );
        self.permits += other.permits;
        other.permits = 0;
    }

    /// Splits `n` permits from `self` and returns a new [`OwnedSemaphorePermit`] instance that holds `n` permits.
    ///
    /// If there are insufficient permits and it's not possible to reduce by `n`, returns `None`.
    ///
    /// # Note
    ///
    /// It will clone the owned `Arc<Semaphore>` to construct the new instance.
    pub fn split(&mut self, n: usize) -> Option<Self> {
        let n = u32::try_from(n).ok()?;

        if n > self.permits {
            return None;
        }

        self.permits -= n;

        Some(Self {
            sem: self.sem.clone(),
            permits: n,
        })
    }

    /// Returns the [`Semaphore`] from which this permit was acquired.
    pub fn semaphore(&self) -> &Arc<Semaphore> {
        &self.sem
    }

    /// Returns the number of permits held by `self`.
    pub fn num_permits(&self) -> usize {
        self.permits as usize
    }
}

impl Drop for SemaphorePermit<'_> {
    fn drop(&mut self) {
        self.sem.add_permits(self.permits as usize);
    }
}

impl Drop for OwnedSemaphorePermit {
    fn drop(&mut self) {
        self.sem.add_permits(self.permits as usize);
    }
}
