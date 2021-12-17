use crate::runtime::execution::ExecutionState;
use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::{TaskId, TaskSet};
use crate::runtime::thread;
use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::{LockResult, PoisonError, TryLockError, TryLockResult};
use tracing::trace;

/// A mutex, the same as [`std::sync::Mutex`].
#[derive(Debug)]
pub struct Mutex<T> {
    inner: std::sync::Mutex<T>,
    state: Rc<RefCell<MutexState>>,
}

/// A mutex guard, the same as [`std::sync::MutexGuard`].
#[derive(Debug)]
pub struct MutexGuard<'a, T> {
    inner: Option<std::sync::MutexGuard<'a, T>>,
    mutex: &'a Mutex<T>,
}

#[derive(Debug)]
struct MutexState {
    holder: Option<TaskId>,
    waiters: TaskSet,
    clock: VectorClock,
}

impl<T> Mutex<T> {
    /// Creates a new mutex in an unlocked state ready for use.
    pub fn new(value: T) -> Self {
        let state = MutexState {
            holder: None,
            waiters: TaskSet::new(),
            clock: VectorClock::new(),
        };

        Self {
            inner: std::sync::Mutex::new(value),
            state: Rc::new(RefCell::new(state)),
        }
    }

    /// Acquires a mutex, blocking the current thread until it is able to do so.
    pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        let me = ExecutionState::me();

        let mut state = self.state.borrow_mut();
        trace!(holder=?state.holder, waiters=?state.waiters, "waiting to acquire mutex {:p}", self);

        // We are waiting for the lock
        state.waiters.insert(me);
        // If the lock is already held, then we are blocked
        if let Some(holder) = state.holder {
            assert_ne!(holder, me);
            ExecutionState::with(|s| s.current_mut().block());
        }
        drop(state);

        // Acquiring a lock is a yield point
        thread::switch();

        let mut state = self.state.borrow_mut();
        // Once the scheduler has resumed this thread, we are clear to become its holder.
        assert!(state.waiters.remove(me));
        assert!(state.holder.is_none());
        state.holder = Some(me);

        trace!(waiters=?state.waiters, "acquired mutex {:p}", self);

        // Re-block all other waiting threads, since we won the race to take this lock
        for tid in state.waiters.iter() {
            ExecutionState::with(|s| s.get_mut(tid).block());
        }
        // Update acquiring thread's clock with the clock stored in the Mutex
        ExecutionState::with(|s| s.update_clock(&state.clock));
        drop(state);

        // Grab a `MutexGuard` from the inner lock, which we must be able to acquire here
        match self.inner.try_lock() {
            Ok(guard) => Ok(MutexGuard {
                inner: Some(guard),
                mutex: self,
            }),
            Err(TryLockError::Poisoned(guard)) => Err(PoisonError::new(MutexGuard {
                inner: Some(guard.into_inner()),
                mutex: self,
            })),
            Err(TryLockError::WouldBlock) => panic!("mutex state out of sync"),
        }
    }

    /// Attempts to acquire this lock.
    ///
    /// If the lock could not be acquired at this time, then Err is returned. This function does not
    /// block.
    pub fn try_lock(&self) -> TryLockResult<MutexGuard<T>> {
        unimplemented!()
    }

    /// Consumes this mutex, returning the underlying data.
    pub fn into_inner(self) -> LockResult<T> {
        let state = self.state.borrow();
        assert!(state.holder.is_none());
        assert!(state.waiters.is_empty());
        // Update the receiver's clock with the Mutex clock
        ExecutionState::with(|s| {
            s.update_clock(&state.clock);
        });
        self.inner.into_inner()
    }
}

// Safety: Mutex is never actually passed across true threads, only across continuations. The
// Rc<RefCell<_>> type therefore can't be preempted mid-bookkeeping-operation.
// TODO we shouldn't need to do this, but RefCell is not Send, and anything we put within a Mutex
// TODO needs to be Send.
unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<'a, T> MutexGuard<'a, T> {
    /// Release the lock, but return a reference to it so it can be re-acquired later
    pub(super) fn unlock(self) -> &'a Mutex<T> {
        self.mutex
    }
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        self.inner = None;

        let mut state = self.mutex.state.borrow_mut();
        trace!(waiters=?state.waiters, "releasing mutex {:p}", self.mutex);
        state.holder = None;

        // Bail out early if we're panicking so we don't try to touch `ExecutionState`
        if ExecutionState::should_stop() {
            return;
        }

        // Unblock every thread waiting on this lock. The scheduler will choose one of them to win
        // the race to this lock, and that thread will re-block all the losers.
        let me = ExecutionState::me();
        state.holder = None;
        for tid in state.waiters.iter() {
            debug_assert_ne!(tid, me);
            ExecutionState::with(|s| {
                let t = s.get_mut(tid);
                debug_assert!(t.blocked());
                t.unblock();
            });
        }

        // Update the Mutex clock with the releasing thread's clock
        ExecutionState::with(|s| {
            let clock = s.increment_clock();
            state.clock.update(clock);
        });

        drop(state);

        // Releasing a lock is a yield point
        thread::switch();
    }
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &**self.inner.as_ref().unwrap()
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut **self.inner.as_mut().unwrap()
    }
}
