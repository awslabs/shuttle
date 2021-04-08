use bitvec::prelude::*;
use bitvec::vec::BitVec;
use futures::future::BoxFuture;
use futures::task::Waker;
use std::cell::RefCell;
use std::fmt::Debug;
use std::rc::Rc;

pub(crate) mod waker;
use waker::make_waker;

// A note on terminology: we have competing notions of threads floating around. Here's the
// convention for disambiguating them:
// * A "thread" is a user-level unit of concurrency. User code creates threads, passes data
//   between them, etc. There is no notion of "thread" inside the Shuttle executor, which only
//   understands Futures. We implement threads via `ThreadFuture`, which emulates a thread inside
//   a Future using a continuation.
// * A "future" is another user-level unit of concurrency, corresponding directly to Rust's notion
//   in std::future::Future. A future has a single method `poll` that can be used to resume
//   executing its computation.
// * A "task" is the Shuttle executor's reflection of a user-level unit of concurrency. Each task
//   has a corresponding Future, which is the user-level code it runs, as well as a state like
//   "blocked", "runnable", etc. Scheduling algorithms take as input the state of all tasks
//   and decide which task should execute next. A context switch is when one task stops executing
//   and another begins.
// * A "continuation" is a low-level implementation of green threading for concurrency. Each
//   ThreadFuture contains a corresponding continuation. When the Shuttle executor polls a
//   ThreadFuture, which corresponds to a user-level thread, the ThreadFuture resumes its
//   continuation and runs it until that continuation yields, which happens when its thread decides
//   it might want to context switch (e.g., because it's blocked on a lock).

pub(crate) const DEFAULT_INLINE_TASKS: usize = 16;

/// A `Task` represents a user-level unit of concurrency. Each task has an `id` that is unique within
/// the execution, and a `state` reflecting whether the task is runnable (enabled) or not.
pub(crate) struct Task {
    pub(super) id: TaskId,
    pub(super) state: TaskState,
    // We use this to decide whether to block this task if it returns Poll::Pending
    task_type: TaskType,

    pub(super) future: Rc<RefCell<BoxFuture<'static, ()>>>,

    waiter: Option<TaskId>,

    waker: Waker,
    // Remember whether the waker was invoked while we were running so we don't re-block
    woken_by_self: bool,

    pub(super) name: Option<String>,
}

impl Task {
    pub(crate) fn new(future: BoxFuture<'static, ()>, id: TaskId, task_type: TaskType, name: Option<String>) -> Self {
        let waker = make_waker(id);

        Self {
            id,
            state: TaskState::Runnable,
            task_type,

            future: Rc::new(RefCell::new(future)),

            waiter: None,
            waker,
            woken_by_self: false,
            name,
        }
    }

    pub(crate) fn id(&self) -> TaskId {
        self.id
    }

    pub(crate) fn runnable(&self) -> bool {
        self.state == TaskState::Runnable
    }

    pub(crate) fn blocked(&self) -> bool {
        self.state == TaskState::Blocked
    }

    pub(crate) fn finished(&self) -> bool {
        self.state == TaskState::Finished
    }

    pub(crate) fn waker(&self) -> Waker {
        self.waker.clone()
    }

    pub(crate) fn block(&mut self) {
        assert!(self.state != TaskState::Finished);
        self.state = TaskState::Blocked;
    }

    pub(crate) fn unblock(&mut self) {
        // Note we don't assert the task is blocked here. For example, a task invoking its own waker
        // will not be blocked when this is called.
        assert!(self.state != TaskState::Finished);
        self.state = TaskState::Runnable;
    }

    pub(crate) fn finish(&mut self) {
        assert!(self.state != TaskState::Finished);
        self.state = TaskState::Finished;
    }

    /// Potentially block this task after it was polled by the executor.
    ///
    /// A task that wraps a `ThreadFuture` won't be blocked here, because we want threads to be
    /// enabled-by-default to avoid bugs where Shuttle incorrectly omits a potential execution.
    /// We also need to handle a special case where a task invoked its own waker, in which case
    /// we should not block the task.
    pub(crate) fn block_after_running(&mut self) {
        let was_woken_by_self = std::mem::replace(&mut self.woken_by_self, false);
        if self.task_type == TaskType::Future && !was_woken_by_self {
            self.block();
        }
    }

    /// Remember that we have been unblocked while we were currently running, and therefore should
    /// not be blocked again by `block_after_running`.
    pub(super) fn set_woken_by_self(&mut self) {
        self.woken_by_self = true;
    }

    /// Register a waiter for this thread to terminate. Returns a boolean indicating whether the
    /// waiter should block or not. If false, this task has already finished, and so the waiter need
    /// not block.
    pub(crate) fn set_waiter(&mut self, waiter: TaskId) -> bool {
        assert!(self.waiter.is_none());
        if self.finished() {
            false
        } else {
            self.waiter = Some(waiter);
            true
        }
    }

    pub(crate) fn take_waiter(&mut self) -> Option<TaskId> {
        self.waiter.take()
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum TaskState {
    Runnable,
    Blocked,
    Finished,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum TaskType {
    Thread,
    Future,
}

/// A `TaskId` is a unique identifier for a task. `TaskId`s are never reused within a single
/// execution.
#[derive(PartialEq, Eq, Hash, Clone, Copy, PartialOrd, Ord, Debug)]
pub struct TaskId(pub(super) usize);

impl From<usize> for TaskId {
    fn from(id: usize) -> Self {
        TaskId(id)
    }
}

impl From<TaskId> for usize {
    fn from(tid: TaskId) -> usize {
        tid.0
    }
}

/// A `TaskSet` is a set of `TaskId`s but implemented efficiently as an array of bools.
// TODO this probably won't work well with large numbers of tasks -- maybe a BitVec?
#[derive(PartialEq, Eq)]
pub(crate) struct TaskSet {
    tasks: BitVec,
}

impl TaskSet {
    pub fn new() -> Self {
        Self {
            tasks: BitVec::from_bitslice(bits![0; DEFAULT_INLINE_TASKS]),
        }
    }

    pub fn contains(&self, tid: TaskId) -> bool {
        // Return false if tid is outside the TaskSet
        (tid.0 < self.tasks.len()) && self.tasks[tid.0]
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.iter().all(|b| !*b)
    }

    pub fn insert(&mut self, tid: TaskId) {
        if tid.0 >= self.tasks.len() {
            self.tasks.resize(1 + tid.0, false);
        }
        assert!(self.tasks.len() > tid.0);
        *self.tasks.get_mut(tid.0).unwrap() = true;
    }

    pub fn remove(&mut self, tid: TaskId) -> bool {
        std::mem::replace(&mut self.tasks.get_mut(tid.0).unwrap(), false)
    }

    pub fn iter(&self) -> impl Iterator<Item = TaskId> + '_ {
        self.tasks
            .iter()
            .enumerate()
            .filter(|(_, b)| **b)
            .map(|(i, _)| TaskId(i))
    }
}

impl Debug for TaskSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TaskSet {{ ")?;
        for t in self.iter() {
            write!(f, "{} ", t.0)?;
        }
        write!(f, "}}")
    }
}
