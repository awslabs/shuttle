use crate::runtime::execution::ExecutionState;
use crate::runtime::task::TaskId;
use futures::task::ArcWake;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A `Waker` (created via `ArcWake`) that just tells the `ExecutionState` to unblock the
/// given task, and does a little bookkeeping to handle a special case where we try to
/// wake the current task while it's running.
#[derive(Debug)]
pub(crate) struct TaskUnblockingWaker {
    task_id: TaskId,
    // There's a special case where a task might invoke its own waker before returning
    // Poll::Pending, in which case we shouldn't block the task after it returns.
    // (See `asynch::yield_now()` for an example).
    pub(super) woken_by_self: AtomicBool,
}

impl TaskUnblockingWaker {
    pub(crate) fn new(task_id: TaskId) -> Self {
        Self {
            task_id,
            woken_by_self: AtomicBool::new(false),
        }
    }
}

impl ArcWake for TaskUnblockingWaker {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        ExecutionState::with(|state| {
            let waiter = state.get_mut(arc_self.task_id);
            waiter.unblock();

            if state.current().id() == arc_self.task_id {
                arc_self.woken_by_self.store(true, Ordering::SeqCst);
            }
        });
    }
}
