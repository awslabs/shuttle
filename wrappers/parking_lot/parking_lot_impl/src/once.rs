//! A Shuttle model of [`parking_lot`]'s `Once`.
//!
//! [`parking_lot`]: <https://crates.io/crates/parking_lot>

use crate::per_execution::Handle;
use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::thread::switch;
use shuttle_engine::sync_types::{ResourceSignature, ResourceType};
use std::cell::{Cell, RefCell};
use std::fmt;
use std::rc::Rc;
use std::thread;
use tracing::trace;

/// Current state of a `Once`. Mirrors `parking_lot::OnceState`.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum OnceState {
    /// A closure has not been executed yet
    New,

    /// A closure was executed but panicked.
    Poisoned,

    /// A thread is currently executing a closure.
    InProgress,

    /// A closure has completed successfully.
    Done,
}

impl OnceState {
    /// Returns whether the associated `Once` has been poisoned.
    ///
    /// Once an initialization routine for a `Once` has panicked it will forever indicate to future
    /// forced initialization routines that it is poisoned.
    #[inline]
    pub fn poisoned(self) -> bool {
        matches!(self, OnceState::Poisoned)
    }

    /// Returns whether the associated `Once` has successfully executed a closure.
    #[inline]
    pub fn done(self) -> bool {
        matches!(self, OnceState::Done)
    }
}

/// A synchronization primitive which can be used to run a one-time initialization, backed by
/// Shuttle. Mirrors `parking_lot::Once`.
///
/// It behaves as `parking_lot`'s does: one closure runs, the callers that come while it runs wait
/// for it, and a closure that panics poisons the `Once`. After that, `call_once` panics with
/// `parking_lot`'s message, and `call_once_force` runs its closure, which is told that the `Once` was
/// poisoned.
///
/// Its progress belongs to the running execution, so a `static` `Once` runs a closure again in
/// every execution, as `shuttle::sync::Once` and `shuttle::lazy_static!` do. A real `static Once`
/// runs one closure per process.
///
/// Scheduling points: none for a `call_once` or `call_once_force` that finds a closure completed.
/// Otherwise one where the call takes the lock that racing closures contend on, and one where it
/// releases it. [`state`](Self::state) is one, as a load of the state is in `parking_lot`, so a task
/// that waits for the `Once` by checking it in a loop lets the closure run.
pub struct Once {
    handle: Handle,
}

/// What a `Once` has done in one execution.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Phase {
    New,
    Poisoned,
    InProgress,
    Done,
}

/// A `Once`'s state in one execution.
struct State {
    phase: Cell<Phase>,
    /// One permit, held while a closure runs, and taken in turn by the callers that came while it
    /// ran, to see how it went.
    lock: BatchSemaphore,
    /// The clock of the closure that completed, which a caller that finds the `Once` done without
    /// taking the lock joins.
    done_clock: RefCell<VectorClock>,
}

impl Once {
    /// Creates a new `Once` value.
    #[inline]
    pub const fn new() -> Once {
        Once { handle: Handle::new() }
    }

    fn cell(&self) -> Rc<State> {
        self.handle.state(|| State {
            phase: Cell::new(Phase::New),
            // `parking_lot` wakes every waiting thread when a closure finishes, and they race, so
            // an unfair semaphore: after a panic, any of the waiting `call_once_force`s can run
            // its closure next.
            lock: BatchSemaphore::const_new_with_signature(
                1,
                Fairness::Unfair,
                ResourceSignature::new_const(ResourceType::Once),
            ),
            done_clock: RefCell::new(VectorClock::new()),
        })
    }

    /// Returns the current state of this `Once`.
    #[inline]
    pub fn state(&self) -> OnceState {
        switch();
        let state = self.cell();
        match state.phase.get() {
            Phase::New => OnceState::New,
            Phase::Poisoned => OnceState::Poisoned,
            Phase::InProgress => OnceState::InProgress,
            Phase::Done => {
                join(&state.done_clock.borrow());
                OnceState::Done
            }
        }
    }

    /// Performs an initialization routine once and only once. The given closure will be executed if
    /// this is the first time `call_once` has been called, and otherwise the routine will *not* be
    /// invoked.
    ///
    /// This method will block the calling thread if another initialization routine is currently
    /// running.
    ///
    /// When this function returns, it is guaranteed that some initialization has run and completed
    /// (it may not be the closure specified). It is also guaranteed that any memory writes
    /// performed by the executed closure can be reliably observed by other threads at this point
    /// (there is a happens-before relation between the closure and code executing after the
    /// return).
    ///
    /// # Panics
    ///
    /// The closure `f` will only be executed once if this is called concurrently amongst many
    /// threads. If that closure panics, however, then it will *poison* this `Once` instance, causing
    /// all future invocations of `call_once` to also panic.
    pub fn call_once<F>(&self, f: F)
    where
        F: FnOnce(),
    {
        let mut f = Some(f);
        self.call_once_inner(false, &mut |_| (f.take().expect("the closure runs once"))());
    }

    /// Performs the same function as `call_once` except ignores poisoning.
    ///
    /// If this `Once` has been poisoned (some initialization panicked) then this function will
    /// continue to attempt to call initialization functions until one of them doesn't panic.
    ///
    /// The closure `f` is yielded a structure which can be used to query the state of this `Once`
    /// (whether initialization has previously panicked or not).
    pub fn call_once_force<F>(&self, f: F)
    where
        F: FnOnce(OnceState),
    {
        let mut f = Some(f);
        self.call_once_inner(true, &mut |state| (f.take().expect("the closure runs once"))(state));
    }

    fn call_once_inner(&self, ignore_poison: bool, f: &mut dyn FnMut(OnceState)) {
        let state = self.cell();
        match state.phase.get() {
            // The fast path, as in `parking_lot`, and as `shuttle::sync::Once` has it: no scheduling
            // point, just the closure's clock.
            Phase::Done => return join(&state.done_clock.borrow()),
            // `parking_lot` panics here too, without waiting.
            Phase::Poisoned if !ignore_poison => panic!("Once instance has previously been poisoned"),
            _ => {}
        }

        state.lock.acquire_blocking(1).unwrap_or_else(|_| {
            // Nothing closes the semaphore, so it can only be closed while unwinding a panic.
            if !thread::panicking() {
                unreachable!()
            }
        });
        // What the closure that ran while this caller waited did. Acquiring the lock joined its
        // clock.
        match state.phase.get() {
            Phase::Done => return state.lock.release(1),
            Phase::Poisoned if !ignore_poison => {
                state.lock.release(1);
                panic!("Once instance has previously been poisoned");
            }
            Phase::InProgress => unreachable!("a closure runs only while the lock is held"),
            Phase::New | Phase::Poisoned => {}
        }

        let once_state = if state.phase.get() == Phase::Poisoned {
            OnceState::Poisoned
        } else {
            OnceState::New
        };
        trace!(?once_state, "running the closure of Once {:p}", self);
        state.phase.set(Phase::InProgress);

        // A closure that panics poisons the `Once`, and lets the next caller in.
        struct PoisonOnUnwind<'a>(&'a State);
        impl Drop for PoisonOnUnwind<'_> {
            fn drop(&mut self) {
                self.0.phase.set(Phase::Poisoned);
                self.0.lock.release(1);
            }
        }
        let poison_on_unwind = PoisonOnUnwind(&state);
        f(once_state);
        std::mem::forget(poison_on_unwind);

        state.phase.set(Phase::Done);
        *state.done_clock.borrow_mut() = ExecutionState::with(|execution| execution.increment_clock().clone());
        state.lock.release(1);
    }
}

/// Joins `clock` into the running task's.
fn join(clock: &VectorClock) {
    ExecutionState::with(|execution| execution.update_clock(clock));
}

impl Default for Once {
    #[inline]
    fn default() -> Once {
        Once::new()
    }
}

impl fmt::Debug for Once {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Once").field("state", &self.state()).finish()
    }
}
