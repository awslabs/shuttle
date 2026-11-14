//! `Parker` and `Unparker`, which block a Shuttle task rather than an OS thread.

use crate::generation;
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::task::TaskId;
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::thread;
use shuttle_time_inner::time::{Duration, Instant};
use shuttle_time_inner::timer_support::{block_until, peek};
use std::cell::{Cell, RefCell};
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

/// A thread parking primitive.
///
/// Conceptually, each `Parker` has an associated token which is initially not present:
///
/// * The [`park`] method blocks the current thread unless or until the token is available, at
///   which point it automatically consumes the token.
///
/// * The [`park_timeout`] and [`park_deadline`] methods work the same as [`park`], but block for
///   a specified maximum time.
///
/// * The [`unpark`] method atomically makes the token available if it wasn't already. Because the
///   token is initially absent, [`unpark`] followed by [`park`] will result in the second call
///   returning immediately.
///
/// [`park`]: Parker::park
/// [`park_timeout`]: Parker::park_timeout
/// [`park_deadline`]: Parker::park_deadline
/// [`unpark`]: Unparker::unpark
///
/// # Under Shuttle
///
/// A parked task blocks in Shuttle's scheduler, so an execution in which nothing unparks it is
/// reported as a deadlock. The token belongs to the `Parker`, not to the task that parks on it, as
/// in crossbeam, so it is not Shuttle's `thread::park` token and the two do not interfere.
///
/// **A timed park times out only when no task can run** (see
/// [`shuttle_time_inner::timer_support::block_until`]), and the timeout then moves
/// `shuttle-time`'s clock to its deadline. So a `park_timeout` never returns early in a schedule
/// where the unparking task could still have run, and a loop that parks with a timeout while it
/// waits for one is not reported as deadlocked.
///
/// # Differences from `crossbeam_utils::sync::Parker`
///
/// * [`Parker::park_deadline`] takes `shuttle-time`'s `Instant`, not `std::time::Instant`, as the
///   clock it measures is the execution's.
/// * There are no spurious wakeups: a `park` returns because the token was made available or
///   because the wait timed out.
/// * A `Parker` that outlives the execution it was used in starts the next one with no token and
///   nobody waiting. Carrying its state over would leave it holding the identifier of a task that
///   does not exist in the new execution, whose number now means a different task.
pub struct Parker {
    unparker: Unparker,
    _marker: PhantomData<*const ()>,
}

// Safety: as crossbeam's. A `Parker` may be moved to another task, which then parks on it, but it
// is not shared, so that only one task parks on it at a time.
unsafe impl Send for Parker {}

impl Default for Parker {
    fn default() -> Self {
        Self {
            unparker: Unparker {
                inner: Arc::new(Inner {
                    state: RefCell::new(State::Empty),
                    clock: RefCell::new(VectorClock::new()),
                    generation: Cell::new(0),
                }),
            },
            _marker: PhantomData,
        }
    }
}

impl Parker {
    /// Creates a new `Parker`.
    pub fn new() -> Parker {
        Self::default()
    }

    /// Blocks the current thread until the token is made available.
    ///
    /// One scheduling point, and a blocked task if the token is not available. Panics outside of a
    /// Shuttle test.
    pub fn park(&self) {
        self.unparker.inner.park(None);
    }

    /// Blocks the current thread until the token is made available, but only for a limited time.
    ///
    /// The time is measured on `shuttle-time`'s clock: the park times out only when no task can
    /// run.
    pub fn park_timeout(&self, timeout: Duration) {
        match peek().checked_add(timeout) {
            Some(deadline) => self.park_deadline(deadline),
            None => self.park(),
        }
    }

    /// Blocks the current thread until the token is made available, or until a certain deadline.
    ///
    /// The deadline is an instant of `shuttle-time`'s clock: the park times out only when no task
    /// can run.
    pub fn park_deadline(&self, deadline: Instant) {
        self.unparker.inner.park(Some(deadline))
    }

    /// Returns a reference to an associated [`Unparker`].
    pub fn unparker(&self) -> &Unparker {
        &self.unparker
    }

    /// Converts a `Parker` into a raw pointer.
    pub fn into_raw(this: Parker) -> *const () {
        Unparker::into_raw(this.unparker)
    }

    /// Converts a raw pointer into a `Parker`.
    ///
    /// # Safety
    ///
    /// This method is safe to use only with pointers returned by [`Parker::into_raw`].
    pub unsafe fn from_raw(ptr: *const ()) -> Parker {
        Parker {
            // Safety: the caller promises that `ptr` came from `Parker::into_raw`.
            unparker: unsafe { Unparker::from_raw(ptr) },
            _marker: PhantomData,
        }
    }
}

impl fmt::Debug for Parker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Parker { .. }")
    }
}

/// Unparks a thread parked by the associated [`Parker`].
pub struct Unparker {
    inner: Arc<Inner>,
}

// Safety: as crossbeam's. The state is only reached by the tasks of one execution, which Shuttle
// runs on one thread, one at a time, and switches between only at scheduling points, where no
// borrow of the state is held.
unsafe impl Send for Unparker {}
unsafe impl Sync for Unparker {}

impl Unparker {
    /// Atomically makes the token available if it is not already.
    ///
    /// One scheduling point, as the parked task can tell. Panics outside of a Shuttle test.
    pub fn unpark(&self) {
        self.inner.unpark()
    }

    /// Converts an `Unparker` into a raw pointer.
    pub fn into_raw(this: Unparker) -> *const () {
        Arc::into_raw(this.inner).cast::<()>()
    }

    /// Converts a raw pointer into an `Unparker`.
    ///
    /// # Safety
    ///
    /// This method is safe to use only with pointers returned by [`Unparker::into_raw`].
    pub unsafe fn from_raw(ptr: *const ()) -> Unparker {
        Unparker {
            // Safety: the caller promises that `ptr` came from `into_raw`.
            inner: unsafe { Arc::from_raw(ptr.cast::<Inner>()) },
        }
    }
}

impl fmt::Debug for Unparker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Unparker { .. }")
    }
}

impl Clone for Unparker {
    fn clone(&self) -> Unparker {
        Unparker {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// Whether the token is there, and who waits for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// No token, and nobody waiting.
    Empty,
    /// No token, and this task is blocked waiting for one.
    Parked(TaskId),
    /// The token is available.
    Notified,
}

// Safety: as for `Unparker`, whose `Arc` holds it. The state is only reached by the tasks of one
// execution, which Shuttle runs on one thread, one at a time, and switches between only at
// scheduling points, where no borrow of the state is held.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

struct Inner {
    state: RefCell<State>,
    /// The clock of the task that made the token available, which the task that consumes it joins.
    clock: RefCell<VectorClock>,
    /// The execution the state belongs to (see [`generation`]), or zero before the first operation.
    generation: Cell<u64>,
}

impl Inner {
    /// Forgets a state that belongs to an earlier execution: its token was another execution's, and
    /// the task it says is parked does not exist in this one.
    fn start_over_in_a_new_execution(&self) {
        let current = generation::current();
        if self.generation.replace(current) != current {
            *self.state.borrow_mut() = State::Empty;
            *self.clock.borrow_mut() = VectorClock::new();
        }
    }

    fn park(&self, deadline: Option<Instant>) {
        thread::switch();
        self.start_over_in_a_new_execution();
        let me = ExecutionState::me();
        loop {
            {
                let mut state = self.state.borrow_mut();
                match *state {
                    State::Notified => {
                        *state = State::Empty;
                        drop(state);
                        // Take causality from the task that made the token available.
                        let clock = self.clock.borrow().clone();
                        ExecutionState::with(|execution| execution.update_clock(&clock));
                        return;
                    }
                    State::Empty => *state = State::Parked(me),
                    State::Parked(other) => {
                        assert_eq!(other, me, "another task is already parked on this Parker")
                    }
                }
            }
            if deadline.is_some_and(|deadline| peek() >= deadline) {
                // Out of time, so stop waiting for the token.
                let mut state = self.state.borrow_mut();
                if *state == State::Parked(me) {
                    *state = State::Empty;
                }
                return;
            }
            // A park that `unpark` wakes returns here with the state `Notified`, which the next
            // round of the loop consumes.
            block_until(deadline);
        }
    }

    fn unpark(&self) {
        thread::switch();
        self.start_over_in_a_new_execution();
        let clock = ExecutionState::with(|execution| execution.increment_clock().clone());
        self.clock.borrow_mut().update(&clock);
        let mut state = self.state.borrow_mut();
        let parked = match *state {
            // Nobody is waiting, so leave the token for the next `park`.
            State::Empty => None,
            // The token is there already, so this does nothing, as crossbeam's does.
            State::Notified => return,
            State::Parked(task) => Some(task),
        };
        *state = State::Notified;
        drop(state);
        if let Some(task) = parked {
            ExecutionState::with(|execution| {
                if let Some(task) = execution.try_get_mut(task)
                    && !task.finished()
                {
                    task.unblock();
                }
            });
        }
    }
}
