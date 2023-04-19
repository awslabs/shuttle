//! Information about the current thread and current Shuttle execution.
//!
//! This module provides access to information about the current Shuttle execution. It is useful for
//! building tools that need to exploit Shuttle's total ordering of concurrent operations; for
//! example, a tool that wants to check linearizability might want access to a global timestamp for
//! events, which the [`context_switches`] function provides.

use crate::runtime::execution::ExecutionState;
use crate::runtime::task::clock::VectorClock;
pub use crate::runtime::task::{Tag, TaskId};

/// The number of context switches that happened so far in the current Shuttle execution.
///
/// Note that this is the number of *possible* context switches, i.e., including times when the
/// scheduler decided to continue with the same task. This means the result can be used as a
/// timestamp for atomic actions during an execution.
///
/// Panics if called outside of a Shuttle execution.
pub fn context_switches() -> usize {
    ExecutionState::context_switches()
}

/// Get the current thread's vector clock
pub fn clock() -> VectorClock {
    crate::runtime::execution::ExecutionState::with(|state| {
        let me = state.current();
        state.get_clock(me.id()).clone()
    })
}

/// Gets the clock for the thread with the given task ID
pub fn clock_for(task_id: TaskId) -> VectorClock {
    ExecutionState::with(|state| state.get_clock(task_id).clone())
}

/// Sets the `tag` field of the current task.
pub fn set_tag_for_current_task(tag: Tag) {
    ExecutionState::set_tag_for_current_task(tag);
}

/// Gets the `tag` field of the current task.
pub fn get_tag_for_current_task() -> Tag {
    ExecutionState::get_tag_for_current_task()
}

/// Gets the `TaskId` of the current task
/// NOTE: Will panic if there is no current task.
pub fn get_current_task() -> TaskId {
    ExecutionState::me()
}
