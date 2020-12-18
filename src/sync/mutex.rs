use crate::runtime::execution::ExecutionState;
use crate::runtime::task::{TaskId, TaskSet};
use crate::runtime::thread;
use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::{LockResult, TryLockResult};
use tracing::trace;

/// A mutex, the same as `std::sync::Mutex`.
#[derive(Debug)]
pub struct Mutex<T> {
    inner: std::sync::Mutex<T>,
    state: Rc<RefCell<MutexState>>,
}

/// A mutex guard, the same as `std::sync::MutexGuard`.
#[derive(Debug)]
pub struct MutexGuard<'a, T> {
    inner: Option<std::sync::MutexGuard<'a, T>>,
    mutex: &'a Mutex<T>,
}

#[derive(Debug)]
struct MutexState {
    holder: Option<TaskId>,
    waiters: TaskSet,
}

impl<T> Mutex<T> {
    /// Creates a new mutex in an unlocked state ready for use.
    pub fn new(value: T) -> Self {
        let state = MutexState {
            holder: None,
            waiters: TaskSet::new(),
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
        if state.holder.is_some() {
            assert_ne!(state.holder.unwrap(), me);
            ExecutionState::with(|s| s.current_mut().block());
        }
        drop(state);

        // Acquiring a lock is a yield point
        thread::switch();

        let mut state = self.state.borrow_mut();
        // Once the scheduler has resumed this thread, we are clear to become its holder. We might
        // not actually be in the waiters, though (if the lock was uncontended).
        // TODO i think now we are guaranteed to be in the waiters?
        assert!(state.holder.is_none());
        state.holder = Some(me);
        state.waiters.remove(me);
        trace!(waiters=?state.waiters, "acquired mutex {:p}", self);
        // Block all other threads, since we won the race to take this lock
        // TODO a bit of a bummer that we have to do this (it would be cleaner if those threads
        // TODO never become unblocked), but might need to track more state to avoid this.
        for tid in state.waiters.iter() {
            ExecutionState::with(|s| s.get_mut(tid).block());
        }
        drop(state);

        let inner = self.inner.try_lock().expect("mutex state out of sync");

        Ok(MutexGuard {
            inner: Some(inner),
            mutex: self,
        })
    }

    /// Attempts to acquire this lock.
    ///
    /// If the lock could not be acquired at this time, then Err is returned. This function does not
    /// block.
    pub fn try_lock(&self) -> TryLockResult<MutexGuard<T>> {
        unimplemented!()
    }
}

// Safety: Mutex is never actually passed across true threads, only across continuations. The
// Rc<RefCell<_>> type therefore can't be preempted mid-bookkeeping-operation.
// TODO we shouldn't need to do this, but RefCell is not Send, and anything we put within a Mutex
// TODO needs to be Send.
unsafe impl<T> Send for Mutex<T> {}
unsafe impl<T> Sync for Mutex<T> {}

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

        if ExecutionState::should_stop() {
            return;
        }

        // Unblock every thread waiting on this lock. The scheduler will choose one of them to win
        // the race to this lock, and that thread will re-block all the losers.
        let me = ExecutionState::me();
        let mut state = self.mutex.state.borrow_mut();
        state.holder = None;
        for tid in state.waiters.iter() {
            debug_assert_ne!(tid, me);
            ExecutionState::with(|s| {
                let t = s.get_mut(tid);
                debug_assert!(t.blocked());
                t.unblock();
            });
        }
        trace!(waiters=?state.waiters, "releasing mutex {:p}", self.mutex);
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
