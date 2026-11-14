//! A task blocked in a channel operation, and how it blocks.

use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::task::TaskId;
use shuttle_time_inner::time::Instant;
use shuttle_time_inner::timer_support::block_until;
use std::cell::Cell;

/// One blocked call of one task: a send, a receive, a selection or a wait for readiness.
///
/// The call registers it with every operation it waits on. A zero-capacity channel's counterpart
/// that completes or claims one of those operations on the waiter's behalf records which one in
/// `selected`, so that the other operations are no longer on offer.
#[derive(Debug)]
pub struct Waiter {
    task: TaskId,
    selected: Cell<Option<usize>>,
}

impl Waiter {
    /// A waiter for the current task.
    pub(crate) fn new() -> Self {
        Self {
            task: ExecutionState::me(),
            selected: Cell::new(None),
        }
    }

    pub(crate) fn task(&self) -> TaskId {
        self.task
    }

    /// The operation a counterpart completed or claimed on this waiter's behalf, if one did.
    pub(crate) fn selected(&self) -> Option<usize> {
        self.selected.get()
    }

    /// Whether a counterpart can still complete or claim one of the waiter's operations: none has
    /// yet, and the waiter is another task's, as a task cannot rendezvous with itself.
    pub(crate) fn is_available(&self) -> bool {
        self.selected.get().is_none() && self.task != ExecutionState::me()
    }

    /// Records that a counterpart completed or claimed operation `op`.
    pub(crate) fn select(&self, op: usize) {
        debug_assert!(self.selected.get().is_none());
        self.selected.set(Some(op));
    }

    /// Lets the waiter's task run again, to see what changed.
    pub(crate) fn wake(&self) {
        wake(self.task);
    }
}

/// Lets `task` run again, unless it has finished.
pub(crate) fn wake(task: TaskId) {
    ExecutionState::with(|state| {
        if let Some(task) = state.try_get_mut(task)
            && !task.finished()
        {
            task.unblock();
        }
    });
}

/// Blocks the current task until another task wakes it, or until `deadline` passes. Returns
/// whether the wait timed out.
///
/// A wait with a deadline times out only when no task can run (see
/// `shuttle_time_inner::timer_support::block_until`), and the timeout then moves the clock forward
/// to the deadline, so that the program never sees a timeout before its deadline.
pub(crate) fn block(deadline: Option<Instant>) -> bool {
    block_until(deadline)
}
