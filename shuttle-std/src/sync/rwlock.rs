use crate::sync::{ResourceSignature, ResourceType};
use shuttle_engine::future::batch_semaphore::{BatchSemaphore, Fairness};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::per_execution::{PerExecution, ValueLock};
use shuttle_engine::runtime::task::{TaskId, TaskSet};
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
///
/// The state of the lock is kept once for each execution, and the value it protects is not, as for
/// [`Mutex`](crate::sync::Mutex), whose docs say more. Two executions can't hold the value at once,
/// even both to read it: the second one panics.
pub struct RwLock<T: ?Sized> {
    state: PerExecution<RefCell<RwLockState>>,
    semaphore: BatchSemaphore,
    /// Keeps the value to the guards of one execution at a time, which the state, being per
    /// execution, can't.
    value: ValueLock,
    /// The value, as for `Mutex`.
    inner: std::sync::RwLock<T>,
}

#[derive(Debug)]
struct RwLockState {
    holder: RwLockHolder,
    /// Whether a thread of this execution panicked while it held the lock for writing.
    poisoned: bool,
}

/// The name of the lock in the messages of [`ValueLock::acquire`].
const KIND: &str = "RwLock";

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
        Self {
            inner: std::sync::RwLock::new(value),
            semaphore: BatchSemaphore::const_new_with_signature(
                MAX_READS,
                Fairness::Unfair,
                ResourceSignature::new_const(ResourceType::RwLock),
            ),
            value: ValueLock::new(),
            state: PerExecution::new(),
        }
    }
}

impl<T: ?Sized> RwLock<T> {
    /// The state of the lock in the current execution, which the execution creates when it first
    /// uses the lock.
    fn state(&self) -> &RefCell<RwLockState> {
        self.state.get(Self::fresh_state)
    }

    /// The state of an execution that hasn't used the lock yet.
    fn fresh_state() -> RefCell<RwLockState> {
        RefCell::new(RwLockState {
            holder: RwLockHolder::None,
            poisoned: false,
        })
    }

    /// Whether the lock is poisoned in the current execution, where its state can be reached.
    fn poisoned_here(&self) -> bool {
        self.state
            .try_get(Self::fresh_state)
            .and_then(|state| state.try_borrow().ok().map(|state| state.poisoned))
            .unwrap_or(false)
    }

    /// A read guard for the current task, which this execution's state has just let take the lock
    /// for reading, poisoned if the state says so.
    ///
    /// A guard of another execution that holds the value is reported (see [`ValueLock`]). Past
    /// that, the inner `std::sync::RwLock` can only be held by a `Debug` on another thread, for a
    /// moment, as for `Mutex`.
    fn read_guard(&self, poisoned: bool) -> LockResult<RwLockReadGuard<'_, T>> {
        self.value.acquire(KIND);
        let guard = RwLockReadGuard {
            inner: Some(self.inner.read().unwrap_or_else(PoisonError::into_inner)),
            rwlock: self,
            me: ExecutionState::me(),
        };
        if poisoned {
            Err(PoisonError::new(guard))
        } else {
            Ok(guard)
        }
    }

    /// As `read_guard`, for writing.
    fn write_guard(&self, poisoned: bool) -> LockResult<RwLockWriteGuard<'_, T>> {
        self.value.acquire(KIND);
        let guard = RwLockWriteGuard {
            inner: Some(self.inner.write().unwrap_or_else(PoisonError::into_inner)),
            rwlock: self,
            me: ExecutionState::me(),
        };
        if poisoned {
            Err(PoisonError::new(guard))
        } else {
            Ok(guard)
        }
    }

    /// Locks this rwlock with shared read access, blocking the current thread until it can be
    /// acquired.
    pub fn read(&self) -> LockResult<RwLockReadGuard<'_, T>> {
        let poisoned = self.lock(RwLockType::Read);
        self.read_guard(poisoned)
    }

    /// Locks this rwlock with exclusive write access, blocking the current thread until it can
    /// be acquired.
    pub fn write(&self) -> LockResult<RwLockWriteGuard<'_, T>> {
        let poisoned = self.lock(RwLockType::Write);
        self.write_guard(poisoned)
    }

    /// Attempts to acquire this rwlock with shared read access.
    ///
    /// If the access could not be granted at this time, then Err is returned. This function does
    /// not block.
    ///
    /// Note that unlike [`std::sync::RwLock::try_read`], if the current thread already holds this
    /// read lock, `try_read` will return Err.
    pub fn try_read(&self) -> TryLockResult<RwLockReadGuard<'_, T>> {
        // Once the execution's state has granted the lock, this takes it, and reports a guard of
        // another execution that holds the value, as `Mutex::try_lock` does.
        match self.try_lock(RwLockType::Read) {
            Some(poisoned) => Ok(self.read_guard(poisoned)?),
            None => Err(TryLockError::WouldBlock),
        }
    }

    /// Attempts to acquire this rwlock with shared read access.
    ///
    /// If the access could not be granted at this time, then Err is returned. This function does
    /// not block.
    pub fn try_write(&self) -> TryLockResult<RwLockWriteGuard<'_, T>> {
        match self.try_lock(RwLockType::Write) {
            Some(poisoned) => Ok(self.write_guard(poisoned)?),
            None => Err(TryLockError::WouldBlock),
        }
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `RwLock` mutably, no actual locking needs to
    /// take place---the mutable borrow statically guarantees no locks exist.
    #[inline]
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        let poisoned = self.state().borrow().poisoned;
        let value = self.inner.get_mut().unwrap_or_else(PoisonError::into_inner);
        if poisoned {
            Err(PoisonError::new(value))
        } else {
            Ok(value)
        }
    }

    /// Consumes this `RwLock`, returning the underlying data
    pub fn into_inner(self) -> LockResult<T>
    where
        T: Sized,
    {
        let poisoned = {
            let state = self.state().borrow();
            assert_eq!(state.holder, RwLockHolder::None);
            state.poisoned
        };

        // Update the receiver's clock with the RwLock clock
        self.semaphore.try_acquire(MAX_READS).unwrap();

        let value = self.inner.into_inner().unwrap_or_else(PoisonError::into_inner);
        if poisoned {
            Err(PoisonError::new(value))
        } else {
            Ok(value)
        }
    }

    /// Acquire the lock in the provided mode, blocking this thread until it succeeds. Returns
    /// whether the lock is poisoned in this execution.
    fn lock(&self, typ: RwLockType) -> bool {
        let me = ExecutionState::me();

        // The execution's state of the lock, looked up once: for a `static` lock the lookup goes
        // through the execution's storage (see `PerExecution`).
        let lock_state = self.state();

        let mut state = lock_state.borrow_mut();
        trace!(
            holder = ?state.holder,
            semaphore = ?self.semaphore,
            "acquiring {:?} lock on rwlock {:p}",
            typ,
            self,
        );
        drop(state);

        // Detect deadlock due to re-entrancy.
        state = lock_state.borrow_mut();
        assert!(
            match &state.holder {
                RwLockHolder::Write(writer) => *writer != me,
                RwLockHolder::Read(readers) => !readers.contains(me),
                RwLockHolder::None => true,
            },
            "deadlock! task {me:?} tried to acquire a RwLock it already holds"
        );
        drop(state);

        // The first poll of the acquire is the scheduling point before the lock operation. The
        // acquire fails only if the semaphore is closed, which happens only if a release ran after
        // the execution stopped (see `BatchSemaphore::release`). In that case, the lock is taken as
        // if it were free, and the inner `std::sync::RwLock` reports whether it is poisoned.
        let _ = self.semaphore.acquire_blocking(typ.num_permits());

        state = lock_state.borrow_mut();
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
        state.poisoned
    }

    /// Attempt to acquire this lock in the provided mode, but without blocking. Returns whether the
    /// lock is poisoned in this execution if it was able to be acquired without blocking, or `None`
    /// otherwise.
    fn try_lock(&self, typ: RwLockType) -> Option<bool> {
        let me = ExecutionState::me();

        // As in `lock`, looked up once.
        let lock_state = self.state();

        let mut state = lock_state.borrow_mut();
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
            state = lock_state.borrow_mut();
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
            if !acquired {
                // The permit was taken above, and no guard will release it, so release it here.
                // Otherwise the lock loses a reader's worth of permits for the rest of the
                // execution, and once it has lost them all, every later lock operation blocks.
                // Without a scheduling point: no task ran while the permit was taken, so the
                // release puts the semaphore back exactly as the `try_acquire` found it.
                self.semaphore.release_no_scheduling_point(typ.num_permits());
            }
        }

        trace!(
            "{} {:?} lock on rwlock {:p}",
            if acquired { "acquired" } else { "failed to acquire" },
            typ,
            self,
        );

        acquired.then(|| lock_state.borrow().poisoned)
    }

    /// Determines whether the lock is poisoned.
    ///
    /// If another thread is active, the lock can still become poisoned at any time. You should not
    /// trust a `false` value for program correctness without additional synchronization.
    ///
    /// Not a scheduling point: it reads whether another task left the lock poisoned, which that
    /// task cannot observe, exactly as std's relaxed load of the poison flag cannot.
    #[inline]
    pub fn is_poisoned(&self) -> bool {
        self.state().borrow().poisoned
    }

    /// Clear the poisoned state from a lock.
    #[inline]
    pub fn clear_poison(&self) {
        self.state().borrow_mut().poisoned = false;
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
    /// As `std`'s, from the inner `std::sync::RwLock`, which a guard of any execution holds, and
    /// with the current execution's poisoning.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("RwLock");
        match self.inner.try_read() {
            Ok(guard) => d.field("data", &&*guard),
            Err(TryLockError::Poisoned(err)) => d.field("data", &&**err.get_ref()),
            Err(TryLockError::WouldBlock) => d.field("data", &format_args!("<locked>")),
        };
        d.field("poisoned", &self.poisoned_here());
        d.finish_non_exhaustive()
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
        // The guard is done with the value, so it gives it up before the scheduling point of the
        // release below, as `Mutex`'s does. A read guard doesn't poison the lock.
        self.inner = None;
        self.rwlock.value.release();

        // Execution teardown can unwind the task from the yield point in `release` (see
        // `ExecutionState::tear_down`). `release` releases the permit even then, and the rest of
        // unlocking has to happen too, so that the destructors that teardown runs later can lock the
        // `RwLock`.
        struct Unlock<'g, 'a, T: ?Sized>(&'g RwLockReadGuard<'a, T>);
        impl<T: ?Sized> Drop for Unlock<'_, '_, T> {
            #[inline]
            fn drop(&mut self) {
                let guard = self.0;
                // Where the execution's state can't be reached, there is no bookkeeping to do (see
                // `ExecutionState::should_stop`).
                let Some(state) = guard.rwlock.state.try_get(RwLock::<T>::fresh_state) else {
                    return;
                };
                let mut state = state.borrow_mut();
                trace!(
                    holder = ?state.holder,
                    semaphore = ?guard.rwlock.semaphore,
                    "releasing Read lock on rwlock {:p}",
                    guard.rwlock
                );
                let RwLockHolder::Read(readers) = &mut state.holder else {
                    panic!("exiting a reader but rwlock is in the wrong state {:?}", state.holder);
                };
                assert!(readers.remove(guard.me));
                if readers.is_empty() {
                    state.holder = RwLockHolder::None;
                }
                drop(state);
            }
        }
        let unlock = Unlock(self);

        unlock.0.rwlock.semaphore.release(RwLockType::Read.num_permits());
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
        // As for `RwLockReadGuard`, and the poisoning as for `Mutex`'s guard.
        self.inner = None;
        let poisoned = self.rwlock.inner.is_poisoned();
        if poisoned {
            self.rwlock.inner.clear_poison();
        }
        let poisons = poisoned && !ExecutionState::unwinding_for_teardown();
        self.rwlock.value.release();

        // As for `RwLockReadGuard`.
        struct Unlock<'g, 'a, T: ?Sized> {
            guard: &'g RwLockWriteGuard<'a, T>,
            poisons: bool,
        }
        impl<T: ?Sized> Drop for Unlock<'_, '_, T> {
            #[inline]
            fn drop(&mut self) {
                let guard = self.guard;
                // As for `RwLockReadGuard`.
                let Some(state) = guard.rwlock.state.try_get(RwLock::<T>::fresh_state) else {
                    return;
                };
                let mut state = state.borrow_mut();
                trace!(
                    holder = ?state.holder,
                    semaphore = ?guard.rwlock.semaphore,
                    "releasing Write lock on rwlock {:p}",
                    guard.rwlock
                );
                assert_eq!(state.holder, RwLockHolder::Write(guard.me));
                state.holder = RwLockHolder::None;
                state.poisoned |= self.poisons;
                drop(state);
            }
        }
        let unlock = Unlock { guard: self, poisons };

        unlock.guard.rwlock.semaphore.release(RwLockType::Write.num_permits());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_resource_signature_rwlock() {
        shuttle_schedulers::check_random(
            || {
                let rwlock1 = RwLock::new(0);
                let rwlock2 = RwLock::new(0);
                assert_ne!(rwlock1.semaphore.signature(), rwlock2.semaphore.signature());
            },
            1,
        );
    }
}
