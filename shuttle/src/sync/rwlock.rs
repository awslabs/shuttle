use crate::future::batch_semaphore::{BatchSemaphore, Fairness};
use crate::runtime::execution::ExecutionState;
use crate::runtime::task::{TaskId, TaskSet};
use crate::sync::{ResourceSignature, ResourceSignatureData};
use std::cell::RefCell;
use std::fmt::{Debug, Display};
use std::ops::{Deref, DerefMut};
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::{LockResult, PoisonError, TryLockError, TryLockResult};
use tracing::trace;

/// (Theoretical) max number of readers holding the same `RwLock`. Based on
/// the `tokio` implementation.
const MAX_READS: usize = (u32::MAX >> 3) as usize;

/// A reader-writer lock, the same as [`std::sync::RwLock`].
///
/// Unlike [`std::sync::RwLock`], the same thread is never allowed to acquire the read side of a
/// `RwLock` more than once. The `std` version is ambiguous about what behavior is allowed here, so
/// we choose the most conservative one.
pub struct RwLock<T: ?Sized> {
    state: RefCell<RwLockState>,
    semaphore: BatchSemaphore,
    inner: std::sync::RwLock<T>,
}

#[derive(Debug)]
struct RwLockState {
    holder: RwLockHolder,
}

#[derive(PartialEq, Eq, Debug)]
enum RwLockHolder {
    Read(TaskSet),
    Write(TaskId),
    None,
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
enum RwLockType {
    Read,
    Write,
}

impl RwLockType {
    /// Number of semaphore permits corresponding to the given lock type.
    fn num_permits(&self) -> usize {
        match self {
            Self::Read => 1,
            Self::Write => MAX_READS,
        }
    }
}

impl<T> RwLock<T> {
    /// Create a new instance of an `RwLock<T>` which is unlocked.
    #[track_caller]
    pub const fn new(value: T) -> Self {
        let state = RwLockState {
            holder: RwLockHolder::None,
        };

        Self {
            inner: std::sync::RwLock::new(value),
            semaphore: BatchSemaphore::const_new_internal(
                MAX_READS,
                Fairness::Unfair,
                ResourceSignature::RwLock(ResourceSignatureData::new_const()),
            ),
            state: RefCell::new(state),
        }
    }
}

impl<T: ?Sized> RwLock<T> {
    /// Locks this rwlock with shared read access, blocking the current thread until it can be
    /// acquired.
    pub fn read(&self) -> LockResult<RwLockReadGuard<'_, T>> {
        self.lock(RwLockType::Read);

        match self.inner.try_read() {
            Ok(guard) => Ok(RwLockReadGuard {
                inner: Some(guard),
                rwlock: self,
                me: ExecutionState::me(),
            }),
            Err(TryLockError::Poisoned(err)) => Err(PoisonError::new(RwLockReadGuard {
                inner: Some(err.into_inner()),
                rwlock: self,
                me: ExecutionState::me(),
            })),
            Err(TryLockError::WouldBlock) => panic!("rwlock state out of sync"),
        }
    }

    /// Locks this rwlock with exclusive write access, blocking the current thread until it can
    /// be acquired.
    pub fn write(&self) -> LockResult<RwLockWriteGuard<'_, T>> {
        self.lock(RwLockType::Write);

        match self.inner.try_write() {
            Ok(guard) => Ok(RwLockWriteGuard {
                inner: Some(guard),
                rwlock: self,
                me: ExecutionState::me(),
            }),
            Err(TryLockError::Poisoned(err)) => Err(PoisonError::new(RwLockWriteGuard {
                inner: Some(err.into_inner()),
                rwlock: self,
                me: ExecutionState::me(),
            })),
            Err(TryLockError::WouldBlock) => panic!("rwlock state out of sync"),
        }
    }

    /// Attempts to acquire this rwlock with shared read access.
    ///
    /// If the access could not be granted at this time, then Err is returned. This function does
    /// not block.
    ///
    /// Note that unlike [`std::sync::RwLock::try_read`], if the current thread already holds this
    /// read lock, `try_read` will return Err.
    pub fn try_read(&self) -> TryLockResult<RwLockReadGuard<'_, T>> {
        if self.try_lock(RwLockType::Read) {
            match self.inner.try_read() {
                Ok(guard) => Ok(RwLockReadGuard {
                    inner: Some(guard),
                    rwlock: self,
                    me: ExecutionState::me(),
                }),
                Err(TryLockError::Poisoned(err)) => Err(TryLockError::Poisoned(PoisonError::new(RwLockReadGuard {
                    inner: Some(err.into_inner()),
                    rwlock: self,
                    me: ExecutionState::me(),
                }))),
                Err(TryLockError::WouldBlock) => panic!("rwlock state out of sync"),
            }
        } else {
            Err(TryLockError::WouldBlock)
        }
    }

    /// Attempts to acquire this rwlock with shared read access.
    ///
    /// If the access could not be granted at this time, then Err is returned. This function does
    /// not block.
    pub fn try_write(&self) -> TryLockResult<RwLockWriteGuard<'_, T>> {
        if self.try_lock(RwLockType::Write) {
            match self.inner.try_write() {
                Ok(guard) => Ok(RwLockWriteGuard {
                    inner: Some(guard),
                    rwlock: self,
                    me: ExecutionState::me(),
                }),
                Err(TryLockError::Poisoned(err)) => Err(TryLockError::Poisoned(PoisonError::new(RwLockWriteGuard {
                    inner: Some(err.into_inner()),
                    rwlock: self,
                    me: ExecutionState::me(),
                }))),
                Err(TryLockError::WouldBlock) => panic!("rwlock state out of sync"),
            }
        } else {
            Err(TryLockError::WouldBlock)
        }
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `RwLock` mutably, no actual locking needs to
    /// take place---the mutable borrow statically guarantees no locks exist.
    #[inline]
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        self.inner.get_mut()
    }

    /// Consumes this `RwLock`, returning the underlying data
    pub fn into_inner(self) -> LockResult<T>
    where
        T: Sized,
    {
        let state = self.state.borrow();
        assert_eq!(state.holder, RwLockHolder::None);

        // Update the receiver's clock with the RwLock clock
        self.semaphore.try_acquire(MAX_READS).unwrap();

        self.inner.into_inner()
    }

    /// Acquire the lock in the provided mode, blocking this thread until it succeeds.
    fn lock(&self, typ: RwLockType) {
        let me = ExecutionState::me();

        let mut state = self.state.borrow_mut();
        trace!(
            holder = ?state.holder,
            semaphore = ?self.semaphore,
            "acquiring {:?} lock on rwlock {:p}",
            typ,
            self,
        );
        drop(state);

        if !self.semaphore.is_closed() {
            // Detect deadlock due to re-entrancy.
            state = self.state.borrow_mut();
            assert!(
                match &state.holder {
                    RwLockHolder::Write(writer) => *writer != me,
                    RwLockHolder::Read(readers) => !readers.contains(me),
                    RwLockHolder::None => true,
                },
                "deadlock! task {me:?} tried to acquire a RwLock it already holds"
            );
            drop(state);

            self.semaphore.acquire_blocking(typ.num_permits()).unwrap();
        }

        state = self.state.borrow_mut();
        match (typ, &mut state.holder) {
            (RwLockType::Write, RwLockHolder::None) => {
                state.holder = RwLockHolder::Write(me);
            }
            (RwLockType::Read, RwLockHolder::None) => {
                let mut readers = TaskSet::new();
                readers.insert(me);
                state.holder = RwLockHolder::Read(readers);
            }
            (RwLockType::Read, RwLockHolder::Read(readers)) => {
                assert!(readers.insert(me));
            }
            _ => {
                panic!(
                    "resumed a waiting {:?} thread while the lock was in state {:?}",
                    typ, state.holder
                );
            }
        }
        trace!(
            holder = ?state.holder,
            semaphore = ?self.semaphore,
            "acquired {:?} lock on rwlock {:p}",
            typ,
            self
        );
        drop(state);
    }

    /// Attempt to acquire this lock in the provided mode, but without blocking. Returns `true` if
    /// the lock was able to be acquired without blocking, or `false` otherwise.
    fn try_lock(&self, typ: RwLockType) -> bool {
        let me = ExecutionState::me();

        let mut state = self.state.borrow_mut();
        trace!(
            holder = ?state.holder,
            semaphore = ?self.semaphore,
            "trying to acquire {:?} lock on rwlock {:p}",
            typ,
            self,
        );
        drop(state);

        // Semaphore is never closed, so an error here is always `NoPermits`.
        let mut acquired = self.semaphore.try_acquire(typ.num_permits()).is_ok();
        if acquired {
            state = self.state.borrow_mut();
            match (typ, &mut state.holder) {
                (RwLockType::Write, RwLockHolder::None) => {
                    state.holder = RwLockHolder::Write(me);
                }
                (RwLockType::Read, RwLockHolder::None) => {
                    let mut readers = TaskSet::new();
                    readers.insert(me);
                    state.holder = RwLockHolder::Read(readers);
                }
                (RwLockType::Read, RwLockHolder::Read(readers)) => {
                    // If we already hold the read lock, `insert` returns false, which will cause this
                    // acquisition to fail with `WouldBlock` so we can diagnose potential deadlocks.
                    acquired = readers.insert(me);
                }
                _ => (),
            };
            drop(state);
        }

        trace!(
            "{} {:?} lock on rwlock {:p}",
            if acquired { "acquired" } else { "failed to acquire" },
            typ,
            self,
        );

        acquired
    }
}

// Safety: RwLock is never actually passed across true threads, only across continuations. The
// Rc<RefCell<_>> type therefore can't be preempted mid-bookkeeping-operation.
// TODO we shouldn't need to do this, but RefCell is not Send, and anything we put within a RwLock
// TODO needs to be Send.
unsafe impl<T: Send + ?Sized> Send for RwLock<T> {}
unsafe impl<T: Send + ?Sized> Sync for RwLock<T> {}

// TODO this is the RefCell biting us again
impl<T: ?Sized> UnwindSafe for RwLock<T> {}
impl<T: ?Sized> RefUnwindSafe for RwLock<T> {}

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<T: ?Sized + Debug> Debug for RwLock<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.inner, f)
    }
}

/// RAII structure used to release the shared read access of a `RwLock` when dropped.
pub struct RwLockReadGuard<'a, T: ?Sized> {
    inner: Option<std::sync::RwLockReadGuard<'a, T>>,
    rwlock: &'a RwLock<T>,
    me: TaskId,
}

impl<T: ?Sized> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap().deref()
    }
}

impl<T: Debug> Debug for RwLockReadGuard<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        Debug::fmt(&self.inner.as_ref().unwrap(), f)
    }
}

impl<T: Display + ?Sized> Display for RwLockReadGuard<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        (**self).fmt(f)
    }
}

impl<T: ?Sized> Drop for RwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        self.inner = None;

        let mut state = self.rwlock.state.borrow_mut();
        trace!(
            holder = ?state.holder,
            semaphore = ?self.rwlock.semaphore,
            "releasing Read lock on rwlock {:p}",
            self.rwlock
        );
        let RwLockHolder::Read(readers) = &mut state.holder else {
            panic!("exiting a reader but rwlock is in the wrong state {:?}", state.holder);
        };
        assert!(readers.remove(self.me));
        if readers.is_empty() {
            state.holder = RwLockHolder::None;
        }
        drop(state);

        self.rwlock.semaphore.release(RwLockType::Read.num_permits());
    }
}

/// RAII structure used to release the exclusive write access of a `RwLock` when dropped.
pub struct RwLockWriteGuard<'a, T: ?Sized> {
    inner: Option<std::sync::RwLockWriteGuard<'a, T>>,
    rwlock: &'a RwLock<T>,
    me: TaskId,
}

impl<T: ?Sized> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap().deref()
    }
}

impl<T: ?Sized> DerefMut for RwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap().deref_mut()
    }
}

impl<T: Debug> Debug for RwLockWriteGuard<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.inner.as_ref().unwrap(), f)
    }
}

impl<T: Display + ?Sized> Display for RwLockWriteGuard<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        (**self).fmt(f)
    }
}

impl<T: ?Sized> Drop for RwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        self.inner = None;

        let mut state = self.rwlock.state.borrow_mut();
        trace!(
            holder = ?state.holder,
            semaphore = ?self.rwlock.semaphore,
            "releasing Write lock on rwlock {:p}",
            self.rwlock
        );
        assert_eq!(state.holder, RwLockHolder::Write(self.me));
        state.holder = RwLockHolder::None;
        drop(state);

        self.rwlock.semaphore.release(RwLockType::Write.num_permits());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_resource_signature_rwlock() {
        crate::check_random(
            || {
                let rwlock1 = RwLock::new(0);
                let rwlock2 = RwLock::new(0);
                assert_ne!(rwlock1.semaphore.signature(), rwlock2.semaphore.signature());
            },
            1,
        );
    }
}
