use futures::future::BoxFuture;
use std::cell::RefCell;
use std::rc::Rc;

mod async_task;
pub(crate) mod serialization;
pub(crate) mod sync_task; // to expose the 'switch' method

// A note on terminology: we have competing notions of threads floating around. Here's the
// convention for disambiguating them:
// * A "thread" is the user-level unit of concurrency. User code creates threads, passes data
//   between them, etc. There is no notion of "thread" inside this runtime. There are other possible
//   user-level units of concurrency, like Futures, but we currently only support threads.
// * A "task" is this runtime's reflection of a user-level unit of concurrency.  A task may be
//   synchronous (spawned thread) or asynchronous (a future).  Each task has a state like
//   "blocked", "runnable", etc. Scheduling algorithms take as input the state of all tasks
//   and decide which task should execute next. A context switch is when one task stops executing
//   and another begins.
// * The low-level implementation of concurrency is either a "Continuation" (for a synchronous task) or
//   a future (for an asynchronous task).
//   When the scheduler decides to execute a synchronous task, we resume its continuation and run
//   until that continuation yields, which happens when its task decides it might want to context
//   switch.
//   When the scheduler decides to execute an asynchronous task, we invoke the poll method on its
//   future.  If the poll method returns Pending, the task is not finished, and we'll poll it again.

// TODO make bigger and configurable
pub(crate) const MAX_TASKS: usize = 4;

#[derive(PartialEq, Eq, Hash, Clone, Copy, PartialOrd, Ord, Debug)]
pub struct TaskId(pub(super) usize);

impl From<usize> for TaskId {
    fn from(id: usize) -> Self {
        TaskId(id)
    }
}

/// A `TaskSet` is a set of `TaskId`s but implemented efficiently as an array of bools.
// TODO this probably won't work well with large numbers of tasks -- maybe a BitVec?
#[derive(PartialEq, Eq, Debug)]
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

/// Results from an atomic step taken by a task
#[derive(Debug)]
pub(crate) enum TaskResult {
    WaitingForFunction, // only used for sync tasks
    Ready,
    Yielded,
    Finished,
}

pub(crate) enum PendingTask {
    SyncTask(Box<dyn FnOnce() + Send + 'static>),
    AsyncTask(BoxFuture<'static, ()>),
}

pub(crate) trait AtomicTask {
    fn step(&mut self) -> TaskResult;
    fn cleanup(self: Box<Self>);
}

/// A `Task` represents a user-level unit of concurrency (currently, that's just a thread). Each
/// task has an `id` that is unique within the execution, and a `state` reflecting whether the task
/// is runnable (enabled) or not.
pub(crate) struct Task {
    pub(crate) id: TaskId,
    pub(crate) state: TaskState,
    pub(crate) waiter: Option<TaskId>,
    pub(crate) inner: Rc<RefCell<Box<dyn AtomicTask>>>,
}

impl Task {
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

    /// Create a task from a pending task
    pub(crate) fn from_pending(task_id: TaskId, pending_task: PendingTask) -> Self {
        let inner: Box<dyn AtomicTask> = match pending_task {
            PendingTask::SyncTask(f) => Box::new(sync_task::new(f)),
            PendingTask::AsyncTask(f) => Box::new(async_task::new(f)),
        };
        Self {
            id: task_id,
            state: TaskState::Runnable,
            waiter: None,
            inner: Rc::new(RefCell::new(inner)),
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum TaskState {
    Runnable,
    Blocked,
    Finished,
}
