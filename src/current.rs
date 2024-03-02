//! Information about the current thread and current Shuttle execution.
//!
//! This module provides access to information about the current Shuttle execution. It is useful for
//! building tools that need to exploit Shuttle's total ordering of concurrent operations; for
//! example, a tool that wants to check linearizability might want access to a global timestamp for
//! events, which the [`context_switches`] function provides.

#[allow(deprecated)]
use crate::runtime::execution::TASK_ID_TO_TAGS;
use crate::runtime::execution::{ExecutionState, LABELS};
use crate::runtime::task::clock::VectorClock;
pub use crate::runtime::task::labels::Labels;
pub use crate::runtime::task::{ChildLabelFn, TaskId, TaskName};
#[allow(deprecated)]
pub use crate::runtime::task::{Tag, Taggable};
use std::fmt::Debug;
use std::sync::Arc;

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
    ExecutionState::with(|state| {
        let me = state.current();
        state.get_clock(me.id()).clone()
    })
}

/// Gets the clock for the thread with the given task ID
pub fn clock_for(task_id: TaskId) -> VectorClock {
    ExecutionState::with(|state| state.get_clock(task_id).clone())
}

/// Apply the given function to the Labels for the specified task
pub fn with_labels_for_task<F, T>(task_id: TaskId, f: F) -> T
where
    F: FnOnce(&mut Labels) -> T,
{
    LABELS.with(|cell| {
        let mut map = cell.borrow_mut();
        let m = map.entry(task_id).or_default();
        f(m)
    })
}

/// Get a label of the given type for the specified task, if any
#[inline]
pub fn get_label_for_task<T: Clone + Debug + Send + Sync + 'static>(task_id: TaskId) -> Option<T> {
    with_labels_for_task(task_id, |labels| labels.get().cloned())
}

/// Add the given label to the specified task, returning the old label for the type, if any
#[inline]
pub fn set_label_for_task<T: Clone + Debug + Send + Sync + 'static>(task_id: TaskId, value: T) -> Option<T> {
    with_labels_for_task(task_id, |labels| labels.insert(value))
}

/// Remove a label of the given type for the specified task, returning the old label for the type, if any
#[inline]
pub fn remove_label_for_task<T: Clone + Debug + Send + Sync + 'static>(task_id: TaskId) -> Option<T> {
    with_labels_for_task(task_id, |labels| labels.remove())
}

/// Get the debug name for a task
#[inline]
pub fn get_name_for_task(task_id: TaskId) -> Option<TaskName> {
    get_label_for_task::<TaskName>(task_id)
}

/// Set the debug name for a task, returning the old name, if any
#[inline]
pub fn set_name_for_task(task_id: TaskId, task_name: impl Into<TaskName>) -> Option<TaskName> {
    set_label_for_task::<TaskName>(task_id, task_name.into())
}

/// Gets the `TaskId` of the current task, or `None` if there is no current task.
pub fn get_current_task() -> Option<TaskId> {
    ExecutionState::with(|s| Some(s.try_current()?.id()))
}

/// Get the `TaskId` of the current task.  Panics if there is no current task.
#[inline]
pub fn me() -> TaskId {
    get_current_task().unwrap()
}

/// Sets the `tag` field of the current task.
/// Returns the `tag` which was there previously.
#[allow(deprecated)]
pub fn set_tag_for_current_task(tag: Arc<dyn Tag>) -> Option<Arc<dyn Tag>> {
    ExecutionState::set_tag_for_current_task(tag)
}

/// Gets the `tag` field of the current task.
#[allow(deprecated)]
pub fn get_tag_for_current_task() -> Option<Arc<dyn Tag>> {
    ExecutionState::get_tag_for_current_task()
}

/// Gets the `tag` field of the specified task.
#[allow(deprecated)]
pub fn get_tag_for_task(task_id: TaskId) -> Option<Arc<dyn Tag>> {
    TASK_ID_TO_TAGS.with(|cell| {
        let map = cell.borrow();
        map.get(&task_id).cloned()
    })
}

/// Sets the `tag` field of the specified task.
#[allow(deprecated)]
pub fn set_tag_for_task(task: TaskId, tag: Arc<dyn Tag>) -> Option<Arc<dyn Tag>> {
    ExecutionState::set_tag_for_task(task, tag)
}
