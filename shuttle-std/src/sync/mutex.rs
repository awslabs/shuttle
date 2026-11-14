use crate::sync::{LockResult, PoisonError, TryLockError, TryLockResult};
use crate::sync::{ResourceSignature, ResourceType};
use shuttle_engine::current;
use shuttle_engine::future::batch_semaphore::{BatchSemaphore, Fairness};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::per_execution::{PerExecution, ValueLock};
use shuttle_engine::runtime::task::TaskId;
use std::cell::RefCell;
use std::fmt::{Debug, Display};
use std::ops::{Deref, DerefMut};
use std::panic::{RefUnwindSafe, UnwindSafe};
use tracing::trace;

/// A mutex, the same as [`std::sync::Mutex`].
///
/// # Differences from `std`
///
/// The state of the lock is kept once for each execution, so that a `static` mutex is unlocked and
/// unpoisoned at the start of every execution, whatever an earlier execution left it in. The value
/// it protects is not: the mutex has only the one value it was created with, and can't make a
/// fresh copy of it for each execution. A `static` mutex that an execution changes the value of
/// passes the change on to the executions after it, which then depend on each other, so that a
/// failing schedule may not replay on its own. To give each execution a fresh value, put the mutex
/// in a [`LazyLock`](crate::sync::LazyLock) or a `shuttle::lazy_static!`, which make it again in
/// each execution.
///
/// Two executions can't hold the value at once. An execution that the mutex's state lets take the
/// lock while a guard of another execution holds the value panics, saying whose guard it is: one of
/// a test that runs at the same time on another thread and shares the mutex in a `static`, or one
/// that an execution which has ended left behind (see
/// `shuttle_engine::runtime::per_execution::ValueLock`).
pub struct Mutex<T: ?Sized> {
    state: PerExecution<RefCell<MutexState>>,
    semaphore: BatchSemaphore,
    /// Keeps the value to one execution at a time, which the state, being per execution, can't.
    value: ValueLock,
    /// The value. Its own locking only gives out the guards that the state and `value` grant, and
    /// its poisoning, which every execution would share, is cleared as soon as it is set, and kept
    /// in the state instead (see `MutexState::poisoned`).
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
    /// Whether a thread of this execution panicked while it held the lock.
    poisoned: bool,
}

/// The name of the lock in the messages of [`ValueLock::acquire`].
const KIND: &str = "Mutex";

impl<T> Mutex<T> {
    /// Creates a new mutex in an unlocked state ready for use.
    #[track_caller]
    pub const fn new(value: T) -> Self {
        Self::new_internal(value, ResourceSignature::new_const(ResourceType::Mutex))
    }

    pub(crate) const fn new_internal(value: T, signature: ResourceSignature) -> Self {
        Self {
            state: PerExecution::new(),
            semaphore: BatchSemaphore::const_new_with_signature(1, Fairness::Unfair, signature),
            value: ValueLock::new(),
            inner: std::sync::Mutex::new(value),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
    /// The state of the lock in the current execution, which the execution creates when it first
    /// uses the mutex.
    fn state(&self) -> &RefCell<MutexState> {
        self.state.get(Self::fresh_state)
    }

    /// The state of an execution that hasn't used the mutex yet.
    fn fresh_state() -> RefCell<MutexState> {
        RefCell::new(MutexState {
            holder: None,
            poisoned: false,
        })
    }

    /// A guard for the current task, which this execution's state has just let take the lock,
    /// poisoned if the state says so.
    ///
    /// A guard of another execution that holds the value is reported (see [`ValueLock`]). Past
    /// that, the inner `std::sync::Mutex` is free: the `ValueLock` keeps out every other execution's
    /// guards, and the state the other tasks of this one. Only a `Debug` on another thread can hold
    /// it, for a moment, and its poisoning is never left set.
    fn guard(&self, poisoned: bool) -> LockResult<MutexGuard<'_, T>> {
        self.value.acquire(KIND);
        let guard = MutexGuard {
            inner: Some(self.inner.lock().unwrap_or_else(PoisonError::into_inner)),
            mutex: self,
        };
        if poisoned {
            Err(PoisonError::new(guard))
        } else {
            Ok(guard)
        }
    }

    /// Whether the lock is poisoned in the current execution, where its state can be reached.
    fn poisoned_here(&self) -> bool {
        self.state
            .try_get(Self::fresh_state)
            .and_then(|state| state.try_borrow().ok().map(|state| state.poisoned))
            .unwrap_or(false)
    }

    /// Acquires a mutex, blocking the current thread until it is able to do so.
    pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        let me = current::me();

        // The execution's state of the lock, looked up once: for a `static` mutex the lookup goes
        // through the execution's storage (see `PerExecution`).
        let lock_state = self.state();

        let mut state = lock_state.borrow_mut();
        trace!(holder=?state.holder, semaphore=?self.semaphore, "waiting to acquire mutex {:p}", self);
        drop(state);

        // Detect deadlock due to re-entrancy.
        state = lock_state.borrow_mut();
        assert!(
            match &state.holder {
                Some(holder) => *holder != me,
                None => true,
            },
            "deadlock! task {me:?} tried to acquire a Mutex it already holds"
        );
        drop(state);

        // The first poll of the acquire is the scheduling point before the lock operation. The
        // acquire fails only if the semaphore is closed, which happens only if a release ran after
        // the execution stopped (see `BatchSemaphore::release`). In that case, the lock is taken as
        // if it were free, and the inner `std::sync::Mutex` reports whether it is poisoned.
        let _ = self.semaphore.acquire_blocking(1);

        state = lock_state.borrow_mut();
        assert!(state.holder.is_none());
        state.holder = Some(me);
        let poisoned = state.poisoned;
        drop(state);

        trace!(semaphore=?self.semaphore, "acquired mutex {:p}", self);

        self.guard(poisoned)
    }

    /// Attempts to acquire this lock.
    ///
    /// If the lock could not be acquired at this time, then Err is returned. This function does not
    /// block.
    pub fn try_lock(&self) -> TryLockResult<MutexGuard<'_, T>> {
        let me = current::me();

        // As in `lock`, looked up once.
        let lock_state = self.state();

        let mut state = lock_state.borrow_mut();
        trace!(holder=?state.holder, semaphore=?self.semaphore, "trying to acquire mutex {:p}", self);
        drop(state);

        // `try_acquire` is a yield point. We need to let other threads in here so they
        // (a) may fail a `try_lock` (in case we acquired), or
        // (b) may release the lock (in case we failed to acquire) so we can succeed in a subsequent `try_lock`.
        self.semaphore.try_acquire(1).map_err(|_| TryLockError::WouldBlock)?;

        state = lock_state.borrow_mut();
        state.holder = Some(me);
        let poisoned = state.poisoned;
        drop(state);

        trace!(semaphore=?self.semaphore, "acquired mutex {:p}", self);

        // The execution's state has granted the lock, so this takes it. A guard of another
        // execution that holds the value is reported, rather than failing the `try_lock`, which
        // would make this execution depend on the other one.
        Ok(self.guard(poisoned)?)
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `Mutex` mutably, no actual locking needs to
    /// take place -- the mutable borrow statically guarantees no locks exist.
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

    /// Consumes this mutex, returning the underlying data.
    pub fn into_inner(self) -> LockResult<T>
    where
        T: Sized,
    {
        let poisoned = {
            let state = self.state().borrow();
            assert!(state.holder.is_none());
            state.poisoned
        };

        // Update the receiver's clock with the Mutex clock
        self.semaphore.try_acquire(1).unwrap();

        let value = self.inner.into_inner().unwrap_or_else(PoisonError::into_inner);
        if poisoned {
            Err(PoisonError::new(value))
        } else {
            Ok(value)
        }
    }

    /// Determines whether the mutex is poisoned.
    ///
    /// If another thread is active, the mutex can still become poisoned at any time. You should
    /// not trust a `false` value for program correctness without additional synchronization.
    ///
    /// Not a scheduling point: it reads whether another task left the mutex poisoned, which that
    /// task cannot observe, exactly as std's relaxed load of the poison flag cannot.
    pub fn is_poisoned(&self) -> bool {
        self.state().borrow().poisoned
    }

    /// Clear the poisoned state from a mutex.
    pub fn clear_poison(&self) {
        self.state().borrow_mut().poisoned = false;
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

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<T: ?Sized + Debug> Debug for Mutex<T> {
    /// As `std`'s, from the inner `std::sync::Mutex`, which a guard of any execution holds, and with
    /// the current execution's poisoning.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("Mutex");
        match self.inner.try_lock() {
            Ok(guard) => d.field("data", &&*guard),
            Err(TryLockError::Poisoned(err)) => d.field("data", &&**err.get_ref()),
            Err(TryLockError::WouldBlock) => d.field("data", &format_args!("<locked>")),
        };
        d.field("poisoned", &self.poisoned_here());
        d.finish_non_exhaustive()
    }
}

impl<'a, T: ?Sized> MutexGuard<'a, T> {
    /// Release the lock, but return a reference to it so it can be re-acquired later
    pub(super) fn unlock(self) -> &'a Mutex<T> {
        self.mutex
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        // The guard is done with the value, so it gives it up first, before the scheduling point of
        // the release below: a test on another thread that takes the mutex while this task is
        // switched out there finds the value free (see `ValueLock`). No other task of this execution
        // can take the lock until the release, so none reaches the value meanwhile.
        //
        // Releasing the inner mutex poisons it if this thread panicked while it held it. Every
        // execution shares the inner mutex, so the poisoning is cleared at once, before the value
        // goes to another execution, and recorded in this execution's state below. Teardown
        // unwinding the task is no panic, which mustn't poison it.
        self.inner = None;
        let poisoned = self.mutex.inner.is_poisoned();
        if poisoned {
            self.mutex.inner.clear_poison();
        }
        let poisons = poisoned && !ExecutionState::unwinding_for_teardown();
        self.mutex.value.release();

        // Execution teardown can unwind the task from the yield point in `release` (see
        // `ExecutionState::tear_down`). `release` releases the permit even then, and the rest of
        // unlocking has to happen too, so that the destructors that teardown runs later can lock the
        // mutex.
        struct Unlock<'g, 'a, T: ?Sized> {
            guard: &'g MutexGuard<'a, T>,
            poisons: bool,
        }
        impl<T: ?Sized> Drop for Unlock<'_, '_, T> {
            #[inline]
            fn drop(&mut self) {
                let mutex = self.guard.mutex;
                // Where the execution's state can't be reached, there is no bookkeeping to do (see
                // `ExecutionState::should_stop`).
                if let Some(state) = mutex.state.try_get(Mutex::<T>::fresh_state) {
                    trace!(semaphore=?mutex.semaphore, "releasing mutex {:p}", mutex);
                    let mut state = state.borrow_mut();
                    state.holder = None;
                    state.poisoned |= self.poisons;
                }
            }
        }
        let unlock = Unlock { guard: self, poisons };

        // Release a permit (this is a yield point)
        unlock.guard.mutex.semaphore.release(1);
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

impl<T> shuttle_engine::annotations::WithName for &Mutex<T> {
    fn with_name_and_kind(self, name: Option<&str>, kind: Option<&str>) -> Self {
        (&self.semaphore).with_name_and_kind(name, kind.or(Some("shuttle::sync::Mutex")));
        self
    }
}

impl<T> shuttle_engine::annotations::WithName for Mutex<T> {
    fn with_name_and_kind(self, name: Option<&str>, kind: Option<&str>) -> Self {
        (&self).with_name_and_kind(name, kind);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_resource_signature_mutex() {
        shuttle_schedulers::check_random(
            || {
                let mutex1 = Mutex::new(0);
                let mutex2 = Mutex::new(0);
                assert_ne!(mutex1.semaphore.signature(), mutex2.semaphore.signature());
            },
            1,
        );
    }
}
