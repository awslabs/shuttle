use crate::runtime::execution::{Execution, TaskId};
use std::cell::RefCell;
use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::{LockResult, TryLockResult};

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
    state: Rc<RefCell<MutexState>>,
}

#[derive(Debug)]
struct MutexState {
    holder: Option<TaskId>,
    waiters: HashSet<TaskId>,
}

impl<T> Mutex<T> {
    /// Creates a new mutex in an unlocked state ready for use.
    pub fn new(value: T) -> Self {
        let state = MutexState {
            holder: None,
            waiters: HashSet::new(),
        };

        Self {
            inner: std::sync::Mutex::new(value),
            state: Rc::new(RefCell::new(state)),
        }
    }

    /// Acquires a mutex, blocking the current thread until it is able to do so.
    pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        let me = Execution::me();
        let mut state = self.state.borrow_mut();
        // We are waiting for the lock
        state.waiters.insert(me);
        // If the lock is already held, then we are blocked
        if state.holder.is_some() {
            assert_ne!(state.holder.unwrap(), me);
            Execution::with_state(|s| s.current_mut().block());
        }
        drop(state);

        // Acquiring a lock is a yield point
        Execution::switch();

        let mut state = self.state.borrow_mut();
        // Once the scheduler has resumed this thread, we are clear to become its holder. We might
        // not actually be in the waiters, though (if the lock was uncontended).
        // TODO i think now we are guaranteed to be in the waiters?
        assert!(state.holder.is_none());
        state.holder = Some(me);
        state.waiters.remove(&me);
        // Block all other threads, since we won the race to take this lock
        // TODO a bit of a bummer that we have to do this (it would be cleaner if those threads
        // TODO never become unblocked), but might need to track more state to avoid this.
        for tid in state.waiters.iter() {
            Execution::with_state(|s| s.get_mut(*tid).block());
        }
        drop(state);

        let inner = self.inner.try_lock().expect("mutex state out of sync");

        Ok(MutexGuard {
            inner: Some(inner),
            state: Rc::clone(&self.state),
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

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        self.inner = None;

        // Unblock every thread waiting on this lock. The scheduler will choose one of them to win
        // the race to this lock, and that thread will re-block all the losers.
        let me = Execution::me();
        let mut state = self.state.borrow_mut();
        state.holder = None;
        for tid in state.waiters.iter() {
            assert_ne!(*tid, me);
            Execution::with_state(|s| s.get_mut(*tid).unblock());
        }
        drop(state);

        // Releasing a lock is a yield point
        Execution::switch();
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
