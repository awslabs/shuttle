use crate::runtime::execution::ExecutionState;
use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::{TaskId, TaskSet};
use crate::runtime::thread;
use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::rc::Rc;
use std::sync::{LockResult, PoisonError, TryLockError, TryLockResult};
use tracing::trace;

/// A reader-writer lock, the same as [`std::sync::RwLock`].
#[derive(Debug)]
pub struct RwLock<T> {
    inner: std::sync::RwLock<T>,
    state: Rc<RefCell<RwLockState>>,
}

#[derive(Debug)]
struct RwLockState {
    holder: RwLockHolder,
    waiting_readers: TaskSet,
    waiting_writers: TaskSet,
    clock: VectorClock,
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

impl<T> RwLock<T> {
    /// Create a new instance of an `RwLock<T>` which is unlocked.
    pub fn new(value: T) -> Self {
        let state = RwLockState {
            holder: RwLockHolder::None,
            waiting_readers: TaskSet::new(),
            waiting_writers: TaskSet::new(),
            clock: VectorClock::new(),
        };

        Self {
            inner: std::sync::RwLock::new(value),
            state: Rc::new(RefCell::new(state)),
        }
    }

    /// Locks this rwlock with shared read access, blocking the current thread until it can be
    /// acquired.
    pub fn read(&self) -> LockResult<RwLockReadGuard<'_, T>> {
        self.lock(RwLockType::Read);

        match self.inner.try_read() {
            Ok(guard) => Ok(RwLockReadGuard {
                inner: Some(guard),
                state: Rc::clone(&self.state),
                me: ExecutionState::me(),
            }),
            Err(TryLockError::Poisoned(err)) => Err(PoisonError::new(RwLockReadGuard {
                inner: Some(err.into_inner()),
                state: Rc::clone(&self.state),
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
                state: Rc::clone(&self.state),
                me: ExecutionState::me(),
            }),
            Err(TryLockError::Poisoned(err)) => Err(PoisonError::new(RwLockWriteGuard {
                inner: Some(err.into_inner()),
                state: Rc::clone(&self.state),
                me: ExecutionState::me(),
            })),
            Err(TryLockError::WouldBlock) => panic!("rwlock state out of sync"),
        }
    }

    /// Attempts to acquire this rwlock with shared read access.
    ///
    /// If the access could not be granted at this time, then Err is returned. This function does
    /// not block.
    pub fn try_read(&self) -> TryLockResult<RwLockReadGuard<T>> {
        unimplemented!()
    }

    /// Attempts to acquire this rwlock with shared read access.
    ///
    /// If the access could not be granted at this time, then Err is returned. This function does
    /// not block.
    pub fn try_write(&self) -> TryLockResult<RwLockWriteGuard<T>> {
        unimplemented!()
    }

    /// Consumes this `RwLock`, returning the underlying data
    pub fn into_inner(self) -> LockResult<T> {
        let state = self.state.borrow();
        assert_eq!(state.holder, RwLockHolder::None);
        // Update the receiver's clock with the RwLock clock
        ExecutionState::with(|s| {
            s.update_clock(&state.clock);
        });
        self.inner.into_inner()
    }

    fn lock(&self, typ: RwLockType) {
        let me = ExecutionState::me();

        let mut state = self.state.borrow_mut();
        trace!(
            holder = ?state.holder,
            waiting_readers = ?state.waiting_readers,
            waiting_writers = ?state.waiting_writers,
            "waiting to acquire {:?} lock on rwlock {:p}",
            typ,
            self.state,
        );

        // We are waiting for the lock
        if typ == RwLockType::Write {
            state.waiting_writers.insert(me);
        } else {
            state.waiting_readers.insert(me);
        }
        // Block if the lock is in a state where we can't acquire it immediately
        match &state.holder {
            RwLockHolder::Write(writer) => {
                assert_ne!(*writer, me);
                ExecutionState::with(|s| s.current_mut().block());
            }
            RwLockHolder::Read(readers) => {
                assert!(!readers.contains(me));
                if typ == RwLockType::Write {
                    ExecutionState::with(|s| s.current_mut().block());
                }
            }
            _ => {}
        }
        drop(state);

        // Acquiring a lock is a yield point
        thread::switch();

        let mut state = self.state.borrow_mut();
        // Once the scheduler has resumed this thread, we are clear to take the lock. We might
        // not actually be in the waiters, though (if the lock was uncontended).
        // TODO should always be in the waiters?
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
                readers.insert(me);
            }
            _ => {
                panic!(
                    "resumed a waiting {:?} thread while the lock was in state {:?}",
                    typ, state.holder
                );
            }
        }
        if typ == RwLockType::Write {
            state.waiting_writers.remove(me);
        } else {
            state.waiting_readers.remove(me);
        }
        trace!(
            holder = ?state.holder,
            waiting_readers = ?state.waiting_readers,
            waiting_writers = ?state.waiting_writers,
            "acquired {:?} lock on rwlock {:p}",
            typ,
            self.state
        );
        // Update acquiring thread's clock with the clock stored in the RwLock
        ExecutionState::with(|s| s.update_clock(&state.clock));

        // Block all other waiters, since we won the race to take this lock
        // TODO a bit of a bummer that we have to do this (it would be cleaner if those threads
        // TODO never become unblocked), but might need to track more state to avoid this.
        Self::block_waiters(&*state, me, typ);
        drop(state);
    }

    fn block_waiters(state: &RwLockState, me: TaskId, typ: RwLockType) {
        // Only block waiting readers if the lock is being acquired by a writer
        if typ == RwLockType::Write {
            for tid in state.waiting_readers.iter() {
                assert_ne!(tid, me);
                ExecutionState::with(|s| s.get_mut(tid).block());
            }
        }
        // Always block any waiting writers
        for tid in state.waiting_writers.iter() {
            assert_ne!(tid, me);
            ExecutionState::with(|s| s.get_mut(tid).block());
        }
    }

    fn unblock_waiters(state: &RwLockState, me: TaskId, drop_type: RwLockType) {
        for tid in state.waiting_readers.iter() {
            debug_assert_ne!(tid, me);
            ExecutionState::with(|s| {
                let t = s.get_mut(tid);
                debug_assert!(drop_type == RwLockType::Read || t.blocked());
                t.unblock();
            });
        }

        // Only unblock waiting writers if there are no exiting readers holding the lock
        if state.holder == RwLockHolder::None {
            for tid in state.waiting_writers.iter() {
                debug_assert_ne!(tid, me);
                ExecutionState::with(|s| {
                    let t = s.get_mut(tid);
                    debug_assert!(t.blocked());
                    t.unblock();
                });
            }
        }
    }
}

// Safety: RwLock is never actually passed across true threads, only across continuations. The
// Rc<RefCell<_>> type therefore can't be preempted mid-bookkeeping-operation.
// TODO we shouldn't need to do this, but RefCell is not Send, and anything we put within a RwLock
// TODO needs to be Send.
unsafe impl<T: Send> Send for RwLock<T> {}
unsafe impl<T: Send> Sync for RwLock<T> {}

// TODO this is the RefCell biting us again
impl<T> UnwindSafe for RwLock<T> {}
impl<T> RefUnwindSafe for RwLock<T> {}

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

/// RAII structure used to release the shared read access of a `RwLock` when dropped.
#[derive(Debug)]
pub struct RwLockReadGuard<'a, T> {
    inner: Option<std::sync::RwLockReadGuard<'a, T>>,
    state: Rc<RefCell<RwLockState>>,
    me: TaskId,
}

impl<T> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap().deref()
    }
}

impl<T> Drop for RwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        self.inner = None;

        let mut state = self.state.borrow_mut();

        trace!(
            holder = ?state.holder,
            waiting_readers = ?state.waiting_readers,
            waiting_writers = ?state.waiting_writers,
            "releasing Read lock on rwlock {:p}",
            self.state
        );

        match &mut state.holder {
            RwLockHolder::Read(readers) => {
                let was_reader = readers.remove(self.me);
                assert!(was_reader);
                if readers.is_empty() {
                    state.holder = RwLockHolder::None;
                }
            }
            _ => panic!("exiting a reader but rwlock is in the wrong state"),
        }

        if ExecutionState::should_stop() {
            return;
        }

        // Unblock every thread waiting on this lock. The scheduler will choose one of them to win
        // the race to this lock, and that thread will re-block all the losers.
        RwLock::<T>::unblock_waiters(&*state, self.me, RwLockType::Read);

        drop(state);

        // Releasing a lock is a yield point
        thread::switch();
    }
}

/// RAII structure used to release the exclusive write access of a `RwLock` when dropped.
#[derive(Debug)]
pub struct RwLockWriteGuard<'a, T> {
    inner: Option<std::sync::RwLockWriteGuard<'a, T>>,
    state: Rc<RefCell<RwLockState>>,
    me: TaskId,
}

impl<T> Drop for RwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        self.inner = None;

        let mut state = self.state.borrow_mut();
        trace!(
            holder = ?state.holder,
            waiting_readers = ?state.waiting_readers,
            waiting_writers = ?state.waiting_writers,
            "releasing Write lock on rwlock {:p}",
            self.state
        );

        assert_eq!(state.holder, RwLockHolder::Write(self.me));
        state.holder = RwLockHolder::None;

        // Update the RwLock clock with the owning thread's clock
        ExecutionState::with(|s| {
            let clock = s.increment_clock();
            state.clock.update(clock);
        });

        if ExecutionState::should_stop() {
            return;
        }

        // Unblock every thread waiting on this lock. The scheduler will choose one of them to win
        // the race to this lock, and that thread will re-block all the losers.
        RwLock::<T>::unblock_waiters(&*state, self.me, RwLockType::Write);
        drop(state);

        // Releasing a lock is a yield point
        thread::switch();
    }
}

impl<T> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap().deref()
    }
}

impl<T> DerefMut for RwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap().deref_mut()
    }
}
