use crate::runtime::execution::ExecutionState;
use crate::runtime::task::clock::VectorClock;
use crate::runtime::thread;
use crate::runtime::thread::continuation::{ContinuationPool, PooledContinuation};
use bitvec::prelude::*;
use bitvec::vec::BitVec;
use futures::{task::Waker, Future};
use std::cell::RefCell;
use std::fmt::Debug;
use std::rc::Rc;
use std::task::Context;

pub(crate) mod clock;
pub(crate) mod waker;
use waker::make_waker;

// A note on terminology: we have competing notions of threads floating around. Here's the
// convention for disambiguating them:
// * A "thread" is a user-level unit of concurrency. User code creates threads, passes data
//   between them, etc.
// * A "future" is another user-level unit of concurrency, corresponding directly to Rust's notion
//   in std::future::Future. A future has a single method `poll` that can be used to resume
//   executing its computation. Both futures and threads are implemented in Task,
//   which wraps a continuation that is resumed when the task is scheduled.
// * A "task" is the Shuttle executor's reflection of a user-level unit of concurrency. Each task
//   has a corresponding continuation, which is the user-level code it runs, as well as a state like
//   "blocked", "runnable", etc. Scheduling algorithms take as input the state of all tasks
//   and decide which task should execute next. A context switch is when one task stops executing
//   and another begins.
// * A "continuation" is a low-level implementation of green threading for concurrency. Each
//   Task contains a corresponding continuation. When the Shuttle executor context switches to a
//   Task, the executor resumes that task's continuation until it yields, which happens when its
//   thread decides it might want to context switch (e.g., because it's blocked on a lock).

pub(crate) const DEFAULT_INLINE_TASKS: usize = 16;

/// A `Task` represents a user-level unit of concurrency. Each task has an `id` that is unique within
/// the execution, and a `state` reflecting whether the task is runnable (enabled) or not.
pub(crate) struct Task {
    pub(super) id: TaskId,
    pub(super) state: TaskState,
    // We use this to check `block_unless_self_woken` is only called from a Future task
    task_type: TaskType,

    pub(super) continuation: Rc<RefCell<PooledContinuation>>,

    pub(crate) clock: VectorClock,

    waiter: Option<TaskId>,

    waker: Waker,
    // Remember whether the waker was invoked while we were running so we don't re-block
    woken_by_self: bool,

    name: Option<String>,
}

impl Task {
    /// Create a task from a continuation
    fn new<F>(
        f: F,
        stack_size: usize,
        id: TaskId,
        task_type: TaskType,
        name: Option<String>,
        clock: VectorClock,
    ) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        assert!(id.0 < clock.time.len());
        let mut continuation = ContinuationPool::acquire(stack_size);
        continuation.initialize(Box::new(f));
        let waker = make_waker(id);
        let continuation = Rc::new(RefCell::new(continuation));
        Self {
            id,
            state: TaskState::Runnable,
            task_type,
            continuation,
            clock,
            waiter: None,
            waker,
            woken_by_self: false,
            name,
        }
    }

    pub(crate) fn from_closure<F>(f: F, stack_size: usize, id: TaskId, name: Option<String>, clock: VectorClock) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        Self::new(f, stack_size, id, TaskType::Thread, name, clock)
    }

    pub(crate) fn from_future<F>(
        future: F,
        stack_size: usize,
        id: TaskId,
        name: Option<String>,
        clock: VectorClock,
    ) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let mut future = Box::pin(future);
        Self::new(
            move || {
                let waker = ExecutionState::with(|state| state.current_mut().waker());
                let cx = &mut Context::from_waker(&waker);
                while future.as_mut().poll(cx).is_pending() {
                    ExecutionState::with(|state| {
                        // We need to block before thread::switch() unless we woke ourselves up
                        state.current_mut().block_unless_self_woken();
                    });
                    thread::switch();
                }
            },
            stack_size,
            id,
            TaskType::Future,
            name,
            clock,
        )
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
    /// A synchronous Task should never call this, because we want threads to be
    /// enabled-by-default to avoid bugs where Shuttle incorrectly omits a potential execution.
    /// We also need to handle a special case where a task invoked its own waker, in which case
    /// we should not block the task.
    pub(crate) fn block_unless_self_woken(&mut self) {
        assert!(self.task_type == TaskType::Future);
        let was_woken_by_self = std::mem::replace(&mut self.woken_by_self, false);
        if !was_woken_by_self {
            self.block();
        }
    }

    /// Remember that we have been unblocked while we were currently running, and therefore should
    /// not be blocked again by `block_unless_self_woken`.
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

    pub(crate) fn name(&self) -> Option<String> {
        self.name.clone()
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
