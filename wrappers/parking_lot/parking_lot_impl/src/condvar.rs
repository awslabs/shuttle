//! A Shuttle model of [`parking_lot`]'s `Condvar`.
//!
//! [`parking_lot`]: <https://crates.io/crates/parking_lot>

use crate::mutex::MutexGuard;
use crate::per_execution::Handle;
use crate::raw_mutex::RawMutex;
use shuttle_engine::current::{self, TaskId};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::thread::switch;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::ops::DerefMut;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tracing::trace;

/// A type indicating whether a timed wait on a condition variable returned due to a time out or not.
/// Mirrors `parking_lot::WaitTimeoutResult`.
///
/// Under Shuttle a wait never times out (see [`Condvar`]), so [`timed_out`](Self::timed_out) is
/// always `false`.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct WaitTimeoutResult(bool);

impl WaitTimeoutResult {
    /// Returns whether the wait was known to have timed out.
    #[inline]
    pub fn timed_out(self) -> bool {
        self.0
    }
}

/// A condition variable, backed by Shuttle. Mirrors `parking_lot::Condvar`.
///
/// It behaves as `parking_lot`'s does:
///
/// * A wait unlocks the mutex and starts to wait in one step, so a notification that comes after
///   the unlock can't be missed, and it locks the mutex again before it returns.
/// * There are no spurious wakeups: a wait returns only after a `notify_one` or `notify_all`.
/// * `notify_one` wakes the task that has waited longest, as `parking_lot` wakes the first thread in
///   its queue. It returns whether there was a task to wake; `notify_all` returns how many it woke.
/// * The tasks waiting at a time must all use one mutex. A wait with another one panics with
///   `parking_lot`'s message, before it unlocks anything.
///
/// It differs in one way: **a timed wait never times out.** Shuttle has no clock that a timeout
/// could be measured against, so `wait_for`, `wait_until`, `wait_while_for` and `wait_while_until`
/// wait until they are notified (and for the `while` forms, until the condition is false), and
/// return a [`WaitTimeoutResult`] whose `timed_out` is `false`. This is what Shuttle's
/// `std::sync::Condvar::wait_timeout` does. A wait that would time out in a real run blocks
/// instead, and if nothing ever notifies it, Shuttle reports a deadlock.
///
/// Scheduling points: one where a wait starts (unlock and wait), one where it locks the mutex again,
/// and one before each `notify_one` and `notify_all`.
///
/// The waiters belong to the running execution, not to the condvar, so a `static` condvar starts
/// every execution with nobody waiting.
pub struct Condvar {
    handle: Handle,
}

/// A condvar's state in one execution.
struct State {
    /// The tasks waiting, in the order they started to wait.
    waiters: RefCell<VecDeque<Waiter>>,
    /// The mutex the waiters use, while there are any (see `RawMutex::id`).
    mutex: Cell<Option<usize>>,
}

struct Waiter {
    task: TaskId,
    /// The notifier's clock, once the waiter has been notified: the waiter's wait then happens after
    /// the notification.
    notified: Rc<RefCell<Option<VectorClock>>>,
}

impl Waiter {
    fn wake(&self) {
        *self.notified.borrow_mut() = Some(current::clock());
        ExecutionState::with(|state| state.get_mut(self.task).unblock());
    }
}

impl Condvar {
    /// Creates a new condition variable which is ready to be waited on and notified.
    #[inline]
    pub const fn new() -> Condvar {
        Condvar { handle: Handle::new() }
    }

    fn state(&self) -> Rc<State> {
        self.handle.state(|| State {
            waiters: RefCell::new(VecDeque::new()),
            mutex: Cell::new(None),
        })
    }

    /// Wakes up one blocked thread on this condvar.
    ///
    /// Returns whether a thread was woken up.
    ///
    /// If there is a blocked thread on this condition variable, then it will be woken up from its
    /// call to `wait` or `wait_timeout`. Calls to `notify_one` are not buffered in any way.
    ///
    /// To wake up all threads, see `notify_all()`.
    pub fn notify_one(&self) -> bool {
        switch();
        let state = self.state();
        let Some(waiter) = state.waiters.borrow_mut().pop_front() else {
            trace!("notify_one on condvar {:p} with nobody waiting", self);
            return false;
        };
        if state.waiters.borrow().is_empty() {
            state.mutex.set(None);
        }
        trace!(task = ?waiter.task, "notify_one on condvar {:p}", self);
        waiter.wake();
        true
    }

    /// Wakes up all blocked threads on this condvar.
    ///
    /// Returns the number of threads woken up.
    ///
    /// This method will ensure that any current waiters on the condition variable are awoken. Calls
    /// to `notify_all()` are not buffered in any way.
    ///
    /// To wake up only one thread, see `notify_one()`.
    pub fn notify_all(&self) -> usize {
        switch();
        let state = self.state();
        let waiters = std::mem::take(&mut *state.waiters.borrow_mut());
        state.mutex.set(None);
        trace!(waiters = waiters.len(), "notify_all on condvar {:p}", self);
        for waiter in &waiters {
            waiter.wake();
        }
        waiters.len()
    }

    /// Blocks the current thread until this condition variable receives a notification.
    ///
    /// This function will atomically unlock the mutex specified (represented by `mutex_guard`) and
    /// block the current thread. This means that any calls to `notify_*()` which happen logically
    /// after the mutex is unlocked are candidates to wake this thread up. When this function call
    /// returns, the lock specified will have been re-acquired.
    ///
    /// # Panics
    ///
    /// This function will panic if another thread is waiting on the `Condvar` with a different
    /// `Mutex` object.
    pub fn wait<T: ?Sized>(&self, mutex_guard: &mut MutexGuard<'_, T>) {
        // Safety: the raw mutex is only unlocked and locked again, with the guard borrowed, which
        // `Condvar::wait` in `parking_lot` does too.
        self.wait_internal(unsafe { MutexGuard::mutex(mutex_guard).raw() });
    }

    /// Waits on this condition variable for a notification, timing out after the specified time
    /// instant.
    ///
    /// Under Shuttle this never times out: it is [`wait`](Self::wait), and returns a
    /// [`WaitTimeoutResult`] whose `timed_out` is `false` (see [`Condvar`]).
    ///
    /// # Panics
    ///
    /// This function will panic if another thread is waiting on the `Condvar` with a different
    /// `Mutex` object.
    pub fn wait_until<T: ?Sized>(&self, mutex_guard: &mut MutexGuard<'_, T>, _timeout: Instant) -> WaitTimeoutResult {
        self.wait(mutex_guard);
        WaitTimeoutResult(false)
    }

    /// Waits on this condition variable for a notification, timing out after a specified duration.
    ///
    /// Under Shuttle this never times out: it is [`wait`](Self::wait), and returns a
    /// [`WaitTimeoutResult`] whose `timed_out` is `false` (see [`Condvar`]).
    pub fn wait_for<T: ?Sized>(&self, mutex_guard: &mut MutexGuard<'_, T>, _timeout: Duration) -> WaitTimeoutResult {
        self.wait(mutex_guard);
        WaitTimeoutResult(false)
    }

    /// Blocks the current thread until this condition variable receives a notification. If the
    /// provided condition evaluates to `false`, then the thread is no longer blocked and the
    /// operation is completed. If the condition evaluates to `true`, then the thread is blocked
    /// again and waits for another notification before repeating this process.
    ///
    /// This function will atomically unlock the mutex specified (represented by `mutex_guard`) and
    /// block the current thread. This means that any calls to `notify_*()` which happen logically
    /// after the mutex is unlocked are candidates to wake this thread up. When this function call
    /// returns, the lock specified will have been re-acquired.
    ///
    /// # Panics
    ///
    /// This function will panic if another thread is waiting on the `Condvar` with a different
    /// `Mutex` object.
    pub fn wait_while<T, F>(&self, mutex_guard: &mut MutexGuard<'_, T>, mut condition: F)
    where
        T: ?Sized,
        F: FnMut(&mut T) -> bool,
    {
        while condition(mutex_guard.deref_mut()) {
            self.wait(mutex_guard);
        }
    }

    /// Waits on this condition variable for a notification, timing out after the specified time
    /// instant. If the provided condition evaluates to `false`, then the thread is no longer
    /// blocked and the operation is completed. If the condition evaluates to `true`, then the
    /// thread is blocked again and waits for another notification before repeating this process.
    ///
    /// Under Shuttle this never times out: it is [`wait_while`](Self::wait_while), and returns a
    /// [`WaitTimeoutResult`] whose `timed_out` is `false` (see [`Condvar`]).
    ///
    /// # Panics
    ///
    /// This function will panic if another thread is waiting on the `Condvar` with a different
    /// `Mutex` object.
    pub fn wait_while_until<T, F>(
        &self,
        mutex_guard: &mut MutexGuard<'_, T>,
        condition: F,
        _timeout: Instant,
    ) -> WaitTimeoutResult
    where
        T: ?Sized,
        F: FnMut(&mut T) -> bool,
    {
        self.wait_while(mutex_guard, condition);
        WaitTimeoutResult(false)
    }

    /// Waits on this condition variable for a notification, timing out after a specified duration.
    /// If the provided condition evaluates to `false`, then the thread is no longer blocked and the
    /// operation is completed. If the condition evaluates to `true`, then the thread is blocked
    /// again and waits for another notification before repeating this process.
    ///
    /// Under Shuttle this never times out: it is [`wait_while`](Self::wait_while), and returns a
    /// [`WaitTimeoutResult`] whose `timed_out` is `false` (see [`Condvar`]).
    pub fn wait_while_for<T: ?Sized, F>(
        &self,
        mutex_guard: &mut MutexGuard<'_, T>,
        condition: F,
        _timeout: Duration,
    ) -> WaitTimeoutResult
    where
        F: FnMut(&mut T) -> bool,
    {
        self.wait_while(mutex_guard, condition);
        WaitTimeoutResult(false)
    }

    fn wait_internal(&self, mutex: &RawMutex) {
        // The scheduling point of the wait. Until the task blocks below, no other task runs, so
        // checking the mutex, joining the queue and unlocking are one step, as they are in
        // `parking_lot`, which does them under the lock of its queue.
        switch();
        let state = self.state();
        match state.mutex.get() {
            None => state.mutex.set(Some(mutex.id())),
            Some(id) if id == mutex.id() => {}
            // The mutex is still locked, as in `parking_lot`; the caller's guard unlocks it as the
            // panic unwinds.
            Some(_) => panic!("attempted to use a condition variable with more than one mutex"),
        }
        let me = ExecutionState::me();
        let notified = Rc::new(RefCell::new(None));
        state.waiters.borrow_mut().push_back(Waiter {
            task: me,
            notified: Rc::clone(&notified),
        });
        trace!(task = ?me, "waiting on condvar {:p}", self);
        // Safety: the caller's guard holds the mutex.
        unsafe { mutex.unlock_without_scheduling_point() };
        ExecutionState::with(|state| state.current_mut().block(false));

        // If execution teardown unwinds the task from the switch below (see
        // `ExecutionState::tear_down`), the task is no longer waiting. The caller's guard is dropped
        // as the stack unwinds, and would unlock the mutex that this wait released above, so the
        // mutex is told to ignore that unlock.
        struct StopWaitingOnUnwind<'a> {
            state: &'a State,
            mutex: &'a RawMutex,
            me: TaskId,
        }
        impl Drop for StopWaitingOnUnwind<'_> {
            fn drop(&mut self) {
                let mut waiters = self.state.waiters.borrow_mut();
                waiters.retain(|waiter| waiter.task != self.me);
                if waiters.is_empty() {
                    self.state.mutex.set(None);
                }
                drop(waiters);
                self.mutex.expect_unheld_unlock();
            }
        }
        let stop_waiting_on_unwind = StopWaitingOnUnwind {
            state: &state,
            mutex,
            me,
        };
        switch();
        std::mem::forget(stop_waiting_on_unwind);

        // The notifier took this task off the queue.
        let clock = notified
            .borrow_mut()
            .take()
            .expect("a waiter runs again only once notified");
        ExecutionState::with(|state| state.update_clock(&clock));
        trace!(task = ?me, "woken from condvar {:p}", self);

        // Lock the mutex again, queueing behind the tasks already waiting for it, as `parking_lot`
        // requeues a woken waiter onto the mutex when it is locked.
        lock_api::RawMutex::lock(mutex);
    }
}

impl Default for Condvar {
    #[inline]
    fn default() -> Condvar {
        Condvar::new()
    }
}

impl fmt::Debug for Condvar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Condvar { .. }")
    }
}
