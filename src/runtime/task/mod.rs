use futures::future::BoxFuture;
use std::cell::RefCell;
use std::fmt::Debug;
use std::rc::Rc;

pub(crate) mod serialization;

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

// TODO make bigger and configurable
pub(crate) const MAX_TASKS: usize = 16;

#[derive(PartialEq, Eq, Hash, Clone, Copy, PartialOrd, Ord, Debug)]
pub struct TaskId(pub(super) usize);

impl From<usize> for TaskId {
    fn from(id: usize) -> Self {
        TaskId(id)
    }
}

/// A `TaskSet` is a set of `TaskId`s but implemented efficiently as an array of bools.
// TODO this probably won't work well with large numbers of tasks -- maybe a BitVec?
#[derive(PartialEq, Eq)]
pub(crate) struct TaskSet {
    tasks: [bool; MAX_TASKS],
}

impl TaskSet {
    pub fn new() -> Self {
        Self {
            tasks: [false; MAX_TASKS],
        }
    }

    pub fn contains(&self, tid: TaskId) -> bool {
        self.tasks[tid.0]
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.iter().all(|b| !*b)
    }

    pub fn insert(&mut self, tid: TaskId) {
        self.tasks[tid.0] = true;
    }

    pub fn remove(&mut self, tid: TaskId) -> bool {
        std::mem::replace(&mut self.tasks[tid.0], false)
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

/// Results from an atomic step taken by a task
#[derive(Debug)]
pub(crate) enum TaskResult {
    WaitingForFunction, // only used for sync tasks
    Ready,
    Yielded,
    Finished,
}

/// A `Task` represents a user-level unit of concurrency (currently, that's just a thread). Each
/// task has an `id` that is unique within the execution, and a `state` reflecting whether the task
/// is runnable (enabled) or not.
pub(crate) struct Task {
    pub(super) id: TaskId,
    pub(super) state: TaskState,
    waiter: Option<TaskId>,
    pub(super) future: Rc<RefCell<BoxFuture<'static, ()>>>,
}

impl Task {
    pub(crate) fn new(future: BoxFuture<'static, ()>, id: TaskId) -> Self {
        Self {
            id,
            state: TaskState::Runnable,
            waiter: None,
            future: Rc::new(RefCell::new(future)),
        }
    }

    pub(crate) fn id(&self) -> TaskId {
        self.id
    }

    pub(crate) fn finished(&self) -> bool {
        self.state == TaskState::Finished
    }

    pub(crate) fn blocked(&self) -> bool {
        self.state == TaskState::Blocked
    }

    pub(crate) fn waiter(&self) -> Option<TaskId> {
        self.waiter
    }

    pub(crate) fn block(&mut self) {
        assert_eq!(self.state, TaskState::Runnable);
        self.state = TaskState::Blocked;
    }

    pub(crate) fn unblock(&mut self) {
        assert_eq!(self.state, TaskState::Blocked);
        self.state = TaskState::Runnable;
    }

    /// Like `unblock` but the target doesn't have to be already blocked
    pub(crate) fn maybe_unblock(&mut self) {
        self.state = TaskState::Runnable;
    }

    /// Register a waiter for this thread to terminate. Returns a boolean indicating whether the
    /// waiter should block or not. If false, this task has already finished, and so the waiter need
    /// not block.
    pub(crate) fn wait_for(&mut self, waiter: TaskId) -> bool {
        assert!(self.waiter.is_none());
        if self.finished() {
            false
        } else {
            self.waiter = Some(waiter);
            true
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum TaskState {
    Runnable,
    Blocked,
    Finished,
}
