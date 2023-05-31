use crate::runtime::execution::ExecutionState;
use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::{TaskId, TaskSet};
use crate::runtime::thread;
use std::cell::RefCell;
use std::fmt::{Debug, Display};
use std::ops::{Deref, DerefMut};
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::{LockResult, PoisonError, TryLockError, TryLockResult};
use tracing::trace;

/// A mutex, the same as [`std::sync::Mutex`].
pub struct Mutex<T: ?Sized> {
    state: RefCell<MutexState>,
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
    waiters: TaskSet,
    clock: VectorClock,
}

impl<T> Mutex<T> {
    /// Creates a new mutex in an unlocked state ready for use.
    pub const fn new(value: T) -> Self {
        let state = MutexState {
            holder: None,
            waiters: TaskSet::new(),
            clock: VectorClock::new(),
        };

        Self {
            inner: std::sync::Mutex::new(value),
            state: RefCell::new(state),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Acquires a mutex, blocking the current thread until it is able to do so.
    pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        let me = ExecutionState::me();

        let mut state = self.state.borrow_mut();

        ExecutionState::with(|s| {
            trace!(holder=%s.format_option(&state.holder), waiters=s.format_iter(state.waiters.iter()), "waiting to acquire mutex {:p}", self);
        });

        // If the lock is already held, then we are blocked
        if let Some(holder) = state.holder {
            if holder == me {
                let formatted = ExecutionState::with(|s| s.format_task_id(me));
                panic!("deadlock! task {} tried to acquire a Mutex it already holds", formatted);
            }

            state.waiters.insert(me);
            drop(state);

            // Note that we only need a context switch when we are blocked, but not if the lock is
            // available. Consider that there is another thread `t` that also wants to acquire the
            // lock. At the last context switch (where we were chosen), `t` must have been already
            // runnable and could have been chosen by the scheduler instead. Also, if we want to
            // re-acquire the lock immediately after releasing it, we know that the release had a
            // context switch that allowed other threads to acquire in between.
            ExecutionState::with(|s| s.current_mut().block(false));
            thread::switch();
            state = self.state.borrow_mut();

            // Once the scheduler has resumed this thread, we are clear to become its holder.
            assert!(state.waiters.remove(me));
        }

        assert!(state.holder.is_none());
        state.holder = Some(me);

        ExecutionState::with(|s| {
            trace!(waiters=%s.format_iter(state.waiters.iter()), "acquired mutex {:p}", self);
        });

        ExecutionState::with(|s| {
            // Re-block all other waiting threads, since we won the race to take this lock
            for tid in state.waiters.iter() {
                s.get_mut(tid).block(false);
            }

            // Update acquiring thread's clock with the clock stored in the Mutex
            s.update_clock(&state.clock);

            // Update the vector clock stored in the Mutex with this threads clock.
            // Future threads that fail a `try_lock` have a causal dependency on this thread's acquire.
            state.clock.update(s.get_clock(me));
        });

        drop(state);

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

        // We need to let other threads in here so they may fail a `try_lock`. This is the case
        // because the current thread holding the lock might not have any further context switches
        // until after releasing the lock. The `concurrent_lock_try_lock` test illustrates this
        // scenario and would fail if this context switch is not here.
        thread::switch();

        result
    }

    /// Attempts to acquire this lock.
    ///
    /// If the lock could not be acquired at this time, then Err is returned. This function does not
    /// block.
    pub fn try_lock(&self) -> TryLockResult<MutexGuard<T>> {
        let me = ExecutionState::me();

        let mut state = self.state.borrow_mut();

        ExecutionState::with(|s| {
            trace!(holder=%s.format_option(&state.holder), waiters=s.format_iter(state.waiters.iter()), "trying to acquire mutex {:p}", self);
        });

        // We don't need a context switch here. There are two cases to analyze.
        // * Consider that `state.holder == None` so that we manage to acquire the lock, but that
        //   there is another thread `t` that also wants to acquire. At the last context switch
        //   (where we were chosen), `t` must have been already runnable and could have been chosen
        //   by the scheduler instead. Then `t`'s acquire has a context switch that allows us to
        //   run into the `WouldBlock` case.
        // * Consider that `state.holder == Some(t)` so that we run into the `WouldBlock` case,
        //   but that `t` wants to release. At the last context switch (where we were chosen), `t`
        //   must have been already runnable and could have been chosen by the scheduler instead.
        //   Then `t`'s release has a context switch that allows us to acquire the lock.

        let result = if let Some(holder) = state.holder {
            ExecutionState::with(|s| {
                trace!(
                    "try_lock failed for mutex {:p} held by {}",
                    self,
                    s.format_task_id(holder)
                );
            });
            Err(TryLockError::WouldBlock)
        } else {
            state.holder = Some(me);

            trace!("try_lock acquired mutex {:p}", self);

            // Re-block all other waiting threads, since we won the race to take this lock
            ExecutionState::with(|s| {
                for tid in state.waiters.iter() {
                    s.get_mut(tid).block(false);
                }
            });

            // Grab a `MutexGuard` from the inner lock, which we must be able to acquire here
            match self.inner.try_lock() {
                Ok(guard) => Ok(MutexGuard {
                    inner: Some(guard),
                    mutex: self,
                }),
                Err(TryLockError::Poisoned(guard)) => Err(TryLockError::Poisoned(PoisonError::new(MutexGuard {
                    inner: Some(guard.into_inner()),
                    mutex: self,
                }))),
                Err(TryLockError::WouldBlock) => panic!("mutex state out of sync"),
            }
        };

        ExecutionState::with(|s| {
            // Update the vector clock stored in the Mutex with this threads clock.
            // Future threads that manage to acquire have a causal dependency on this thread's failed `try_lock`.
            // Future threads that fail a `try_lock` have a causal dependency on this thread's successful `try_lock`.
            state.clock.update(s.get_clock(me));

            // Update this thread's clock with the clock stored in the Mutex.
            // We need to do the vector clock update even in the failing case, because there's a causal
            // dependency: if the `try_lock` fails, the current thread `t1` knows that the thread `t2`
            // that owns the lock is in its critical section, and therefore `t1` has a causal dependency
            // on everything that happened before in `t2` (which is recorded in the Mutex's clock).
            s.update_clock(&state.clock);
        });

        drop(state);

        // We need to let other threads in here so they
        // (a) may fail a `try_lock` (in case we acquired), or
        // (b) may release the lock (in case we failed to acquire) so we can succeed in a subsequent `try_lock`.
        thread::switch();

        result
    }

    /// Consumes this mutex, returning the underlying data.
    pub fn into_inner(self) -> LockResult<T>
    where
        T: Sized,
    {
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
        // We don't need a context switch here *before* releasing the lock. There are two cases to analyze.
        // * Other threads that want to `lock` are still blocked at this point.
        // * Other threads that want to `try_lock` and would fail at this point (but not after we release)
        //   were already runnable at the last context switch (which could have been right after we acquired)
        //   and could have been scheduled then to fail the `try_lock`.

        self.inner = None;

        let mut state = self.mutex.state.borrow_mut();

        ExecutionState::with(|s| {
            trace!(waiters=%s.format_iter(state.waiters.iter()), "releasing mutex {:p}", self.mutex);
        });

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
