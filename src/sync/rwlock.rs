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

/// A reader-writer lock, the same as [`std::sync::RwLock`].
///
/// Unlike [`std::sync::RwLock`], the same thread is never allowed to acquire the read side of a
/// `RwLock` more than once. The `std` version is ambiguous about what behavior is allowed here, so
/// we choose the most conservative one.
pub struct RwLock<T: ?Sized> {
    state: RefCell<RwLockState>,
    inner: std::sync::RwLock<T>,
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
    pub const fn new(value: T) -> Self {
        let state = RwLockState {
            holder: RwLockHolder::None,
            waiting_readers: TaskSet::new(),
            waiting_writers: TaskSet::new(),
            clock: VectorClock::new(),
        };

        Self {
            inner: std::sync::RwLock::new(value),
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
    pub fn try_read(&self) -> TryLockResult<RwLockReadGuard<T>> {
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
    pub fn try_write(&self) -> TryLockResult<RwLockWriteGuard<T>> {
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
        ExecutionState::with(|s| {
            s.update_clock(&state.clock);
        });
        self.inner.into_inner()
    }

    /// Acquire the lock in the provided mode, blocking this thread until it succeeds.
    fn lock(&self, typ: RwLockType) {
        let me = ExecutionState::me();

        let mut state = self.state.borrow_mut();
        trace!(
            holder = ?state.holder,
            waiting_readers = ?state.waiting_readers,
            waiting_writers = ?state.waiting_writers,
            "acquiring {:?} lock on rwlock {:p}",
            typ,
            self,
        );

        // We are waiting for the lock
        if typ == RwLockType::Write {
            state.waiting_writers.insert(me);
        } else {
            state.waiting_readers.insert(me);
        }
        // Block if the lock is in a state where we can't acquire it immediately. Note that we only
        // need to context switch here if we can't acquire the lock. If it's available for us to
        // acquire, but there is also another thread `t` that wants to acquire it, then `t` must
        // have been runnable when this thread was chosen to execute and could have been chosen
        // instead.
        let should_switch = match &state.holder {
            RwLockHolder::Write(writer) => {
                if *writer == me {
                    panic!("deadlock! task {:?} tried to acquire a RwLock it already holds", me);
                }
                ExecutionState::with(|s| s.current_mut().block(false));
                true
            }
            RwLockHolder::Read(readers) => {
                if readers.contains(me) {
                    panic!("deadlock! task {:?} tried to acquire a RwLock it already holds", me);
                }
                if typ == RwLockType::Write {
                    ExecutionState::with(|s| s.current_mut().block(false));
                    true
                } else {
                    false
                }
            }
            RwLockHolder::None => false,
        };
        drop(state);

        if should_switch {
            thread::switch();
        }

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
            self
        );

        // Increment the current thread's clock and update this RwLock's clock to match.
        // TODO we can likely do better here: there is no causality between multiple readers holding
        // the lock at the same time.
        ExecutionState::with(|s| {
            s.update_clock(&state.clock);
            state.clock.update(s.get_clock(me));
        });

        // Block all other waiters, since we won the race to take this lock
        Self::block_waiters(&state, me, typ);
        drop(state);

        // We need to let other threads in here so they may fail a `try_read` or `try_write`. This
        // is the case because the current thread holding the lock might not have any further
        // context switches until after releasing the lock.
        thread::switch();
    }

    /// Attempt to acquire this lock in the provided mode, but without blocking. Returns `true` if
    /// the lock was able to be acquired without blocking, or `false` otherwise.
    fn try_lock(&self, typ: RwLockType) -> bool {
        let me = ExecutionState::me();

        let mut state = self.state.borrow_mut();
        trace!(
            holder = ?state.holder,
            waiting_readers = ?state.waiting_readers,
            waiting_writers = ?state.waiting_writers,
            "trying to acquire {:?} lock on rwlock {:p}",
            typ,
            self,
        );

        let acquired = match (typ, &mut state.holder) {
            (RwLockType::Write, RwLockHolder::None) => {
                state.holder = RwLockHolder::Write(me);
                true
            }
            (RwLockType::Read, RwLockHolder::None) => {
                let mut readers = TaskSet::new();
                readers.insert(me);
                state.holder = RwLockHolder::Read(readers);
                true
            }
            (RwLockType::Read, RwLockHolder::Read(readers)) => {
                // If we already hold the read lock, `insert` returns false, which will cause this
                // acquisition to fail with `WouldBlock` so we can diagnose potential deadlocks.
                readers.insert(me)
            }
            _ => false,
        };

        trace!(
            "{} {:?} lock on rwlock {:p}",
            if acquired { "acquired" } else { "failed to acquire" },
            typ,
            self,
        );

        // Update this thread's clock with the clock stored in the RwLock.
        // We need to do the vector clock update even in the failing case, because there's a causal
        // dependency: if the `try_lock` fails, the current thread `t1` knows that the thread `t2`
        // that owns the lock is not in the right state to be read/written, and therefore `t1` has a
        // causal dependency on everything that happened before in `t2` (which is recorded in the
        // RwLock's clock).
        // TODO we can likely do better here: there is no causality between successful `try_read`s
        // and other concurrent readers, and there's no need to update the clock on failed
        // `try_read`s.
        ExecutionState::with(|s| {
            s.update_clock(&state.clock);
            state.clock.update(s.get_clock(me));
        });

        // Block all other waiters, since we won the race to take this lock
        Self::block_waiters(&state, me, typ);
        drop(state);

        // We need to let other threads in here so they
        // (a) may fail a `try_lock` (in case we acquired), or
        // (b) may release the lock (in case we failed to acquire) so we can succeed in a subsequent
        //     `try_lock`.
        thread::switch();

        acquired
    }

    fn block_waiters(state: &RwLockState, me: TaskId, typ: RwLockType) {
        // Only block waiting readers if the lock is being acquired by a writer
        if typ == RwLockType::Write {
            for tid in state.waiting_readers.iter() {
                assert_ne!(tid, me);
                ExecutionState::with(|s| s.get_mut(tid).block(false));
            }
        }
        // Always block any waiting writers
        for tid in state.waiting_writers.iter() {
            assert_ne!(tid, me);
            ExecutionState::with(|s| s.get_mut(tid).block(false));
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
unsafe impl<T: Send + ?Sized> Send for RwLock<T> {}
unsafe impl<T: Send + ?Sized> Sync for RwLock<T> {}

// TODO this is the RefCell biting us again
impl<T: ?Sized> UnwindSafe for RwLock<T> {}
impl<T: ?Sized> RefUnwindSafe for RwLock<T> {}

impl<T: Default + ?Sized> Default for RwLock<T> {
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
            waiting_readers = ?state.waiting_readers,
            waiting_writers = ?state.waiting_writers,
            "releasing Read lock on rwlock {:p}",
            self.rwlock
        );

        match &mut state.holder {
            RwLockHolder::Read(readers) => {
                let was_reader = readers.remove(self.me);
                assert!(was_reader);
                if readers.is_empty() {
                    state.holder = RwLockHolder::None;
                }
            }
            _ => panic!("exiting a reader but rwlock is in the wrong state {:?}", state.holder),
        }

        if ExecutionState::should_stop() {
            return;
        }

        // Unblock every thread waiting on this lock. The scheduler will choose one of them to win
        // the race to this lock, and that thread will re-block all the losers.
        RwLock::<T>::unblock_waiters(&state, self.me, RwLockType::Read);

        drop(state);

        // Releasing a lock is a yield point
        thread::switch();
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
            waiting_readers = ?state.waiting_readers,
            waiting_writers = ?state.waiting_writers,
            "releasing Write lock on rwlock {:p}",
            self.rwlock
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
        RwLock::<T>::unblock_waiters(&state, self.me, RwLockType::Write);
        drop(state);

        // Releasing a lock is a yield point
        thread::switch();
    }
}
