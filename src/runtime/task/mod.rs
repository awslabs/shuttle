use crate::runtime::execution::ExecutionState;
use crate::runtime::storage::{AlreadyDestructedError, StorageKey, StorageMap};
use crate::runtime::task::clock::VectorClock;
use crate::runtime::thread;
use crate::runtime::thread::continuation::{ContinuationPool, PooledContinuation};
use crate::thread::LocalKey;
use bitvec::prelude::*;
use std::any::Any;
use std::cell::RefCell;
use std::fmt::Debug;
use std::future::Future;
use std::rc::Rc;
use std::task::{Context, Waker};

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
#[derive(Debug)]
pub(crate) struct Task {
    pub(super) id: TaskId,
    pub(super) state: TaskState,
    pub(super) detached: bool,
    park_state: ParkState,

    pub(super) continuation: Rc<RefCell<PooledContinuation>>,

    pub(crate) clock: VectorClock,

    waiter: Option<TaskId>,

    waker: Waker,
    // Remember whether the waker was invoked while we were running
    woken: bool,

    name: Option<String>,

    local_storage: StorageMap,
    pub(super) span: tracing::Span,

    // Arbitrarily settable tag which is inherited from the parent.
    tag: Tag,
}

impl Task {
    /// Create a task from a continuation
    #[allow(clippy::too_many_arguments)]
    fn new<F>(
        f: F,
        stack_size: usize,
        id: TaskId,
        name: Option<String>,
        clock: VectorClock,
        parent_span: tracing::Span,
        schedule_len: usize,
        tag: Tag,
    ) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        assert!(id.0 < clock.time.len());
        let mut continuation = ContinuationPool::acquire(stack_size);
        continuation.initialize(Box::new(f));
        let waker = make_waker(id);
        let continuation = Rc::new(RefCell::new(continuation));

        let span = if name == Some("main-thread".to_string()) {
            parent_span
        } else {
            tracing::info_span!(parent: parent_span.id(), "step", i = schedule_len, task = id.0)
        };

        Self {
            id,
            state: TaskState::Runnable,
            continuation,
            clock,
            waiter: None,
            waker,
            woken: false,
            detached: false,
            park_state: ParkState::default(),
            name,
            span,
            local_storage: StorageMap::new(),
            tag,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_closure<F>(
        f: F,
        stack_size: usize,
        id: TaskId,
        name: Option<String>,
        clock: VectorClock,
        parent_span: tracing::Span,
        schedule_len: usize,
        tag: Tag,
    ) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        Self::new(f, stack_size, id, name, clock, parent_span, schedule_len, tag)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_future<F>(
        future: F,
        stack_size: usize,
        id: TaskId,
        name: Option<String>,
        clock: VectorClock,
        parent_span: tracing::Span,
        schedule_len: usize,
        tag: Tag,
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
                    ExecutionState::with(|state| state.current_mut().sleep_unless_woken());
                    thread::switch();
                }
            },
            stack_size,
            id,
            name,
            clock,
            parent_span,
            schedule_len,
            tag,
        )
    }

    pub(crate) fn id(&self) -> TaskId {
        self.id
    }

    pub(crate) fn runnable(&self) -> bool {
        self.state == TaskState::Runnable
    }

    pub(crate) fn blocked(&self) -> bool {
        matches!(self.state, TaskState::Blocked { .. })
    }

    pub(crate) fn can_spuriously_wakeup(&self) -> bool {
        match self.state {
            TaskState::Blocked { allow_spurious_wakeups } => allow_spurious_wakeups,
            _ => false,
        }
    }

    pub(crate) fn sleeping(&self) -> bool {
        self.state == TaskState::Sleeping
    }

    pub(crate) fn finished(&self) -> bool {
        self.state == TaskState::Finished
    }

    pub(crate) fn detach(&mut self) {
        self.detached = true;
    }

    pub(crate) fn waker(&self) -> Waker {
        self.waker.clone()
    }

    /// Block the current thread. If `allow_spurious_wakeups` is true, then the scheduler is
    /// permitted to spuriously wake up the thread (though it will still not count as a live thread
    /// for deadlock detection purposes for as long as it remains blocked).
    pub(crate) fn block(&mut self, allow_spurious_wakeups: bool) {
        assert!(self.state != TaskState::Finished);
        self.state = TaskState::Blocked { allow_spurious_wakeups };
    }

    pub(crate) fn sleep(&mut self) {
        assert!(self.state != TaskState::Finished);
        self.state = TaskState::Sleeping;
    }

    pub(crate) fn unblock(&mut self) {
        // Note we don't assert the task is blocked here. For example, a task invoking its own waker
        // will not be blocked when this is called.
        assert!(self.state != TaskState::Finished);
        self.state = TaskState::Runnable;

        // When a task gets unblocked, it's definitely no longer blocked in a call to `park`. This
        // is necessary to do here because a parked task could be spuriously woken up outside of the
        // `unpark` path. If it later becomes blocked by something else, we don't want a later
        // `unpark` to be able to unblock the task.
        self.park_state.blocked_in_park = false;
    }

    pub(crate) fn finish(&mut self) {
        assert!(self.state != TaskState::Finished);
        self.state = TaskState::Finished;
    }

    /// Potentially put this task to sleep after it was polled by the executor, unless someone has
    /// called its waker first.
    ///
    /// A synchronous Task should never call this, because we want threads to be enabled-by-default
    /// to avoid bugs where Shuttle incorrectly omits a potential execution.
    pub(crate) fn sleep_unless_woken(&mut self) {
        let was_woken = std::mem::replace(&mut self.woken, false);
        if !was_woken {
            self.sleep();
        }
    }

    /// Remember that our waker has been called, and so we should not block the next time the
    /// executor tries to put us to sleep.
    pub(super) fn wake(&mut self) {
        self.woken = true;
        if self.state == TaskState::Sleeping {
            self.unblock();
        }
    }

    /// Register a waiter for this thread to terminate. Returns a boolean indicating whether the
    /// waiter should block or not. If false, this task has already finished, and so the waiter need
    /// not block.
    pub(crate) fn set_waiter(&mut self, waiter: TaskId) -> bool {
        assert!(
            self.waiter.is_none() || self.waiter == Some(waiter),
            "Task cannot have more than one waiter"
        );
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

    /// Retrieve a reference to the given thread-local storage slot.
    ///
    /// Returns Some(Err(_)) if the slot has already been destructed. Returns None if the slot has
    /// not yet been initialized.
    pub(crate) fn local<T: 'static>(&self, key: &'static LocalKey<T>) -> Option<Result<&T, AlreadyDestructedError>> {
        self.local_storage.get(key.into())
    }

    /// Initialize the given thread-local storage slot with a new value.
    ///
    /// Panics if the slot has already been initialized.
    pub(crate) fn init_local<T: 'static>(&mut self, key: &'static LocalKey<T>, value: T) {
        self.local_storage.init(key.into(), value)
    }

    /// Return ownership of the next still-initialized thread-local storage slot, to be used when
    /// running thread-local storage destructors.
    ///
    /// TLS destructors are a little tricky:
    /// 1. Their code can perform synchronization operations (and so require Shuttle to call back
    ///    into ExecutionState), so we can't drop them from within an ExecutionState borrow. Instead
    ///    we move the contents of a slot to the caller to be dropped outside the borrow.
    /// 2. It's valid for destructors to read other TLS slots, although destructor order is
    ///    undefined. This also means it's valid for a destructor to *initialize* another TLS slot.
    ///    To make this work, we run the destructors incrementally, so one destructor can initialize
    ///    another slot that just gets added via `init_local` like normal, and then will be
    ///    available to be popped on a future call to `pop_local`. To prevent an infinite loop, we
    ///    forbid *reinitializing* a TLS slot whose destructor has already run, or is currently
    ///    being run.
    pub(crate) fn pop_local(&mut self) -> Option<Box<dyn Any>> {
        self.local_storage.pop()
    }

    /// Park the task if its park token is unavailable. If the task blocks, then it will be woken up
    /// when the token becomes available or spuriously without consuming the token (see the
    /// documentation for [`std::thread::park`], which says that "it may also return spuriously,
    /// without consuming the token"). Returns true if the execution should switch to a different
    /// task (e.g., if the token was unavailable).
    pub(crate) fn park(&mut self) -> bool {
        assert!(
            !self.park_state.blocked_in_park,
            "task cannot park while already parked"
        );
        assert!(!self.blocked(), "task cannot park while blocked by something else");

        if self.park_state.token_available {
            self.park_state.token_available = false;
            false
        } else {
            self.park_state.blocked_in_park = true;
            self.block(true);
            true
        }
    }

    /// Make the task's park token available, and unblock the task if it was parked.
    pub(crate) fn unpark(&mut self) {
        if self.park_state.blocked_in_park {
            assert!(
                self.blocked() && self.can_spuriously_wakeup(),
                "parked tasks should be blocked"
            );
            assert!(
                !self.park_state.token_available,
                "token shouldn't be available for parked task"
            );

            self.unblock();
        } else {
            // If the thread isn't currently blocked in `park`, then make the token available. If
            // the token already is available, then this does nothing.
            self.park_state.token_available = true;
        }
    }

    pub(crate) fn get_tag(&self) -> Tag {
        self.tag
    }

    pub(crate) fn set_tag(&mut self, tag: Tag) {
        self.tag = tag;
    }
}

/// A `Tag` is an arbitrarily settable value for each task.
#[derive(PartialEq, Eq, Clone, Copy, Debug, Default, Hash, PartialOrd, Ord)]
pub struct Tag(u64);

impl From<u64> for Tag {
    fn from(tag: u64) -> Self {
        Tag(tag)
    }
}

impl From<Tag> for u64 {
    fn from(tag: Tag) -> u64 {
        tag.0
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum TaskState {
    /// Available to be scheduled
    Runnable,
    /// Blocked in a synchronization operation
    Blocked { allow_spurious_wakeups: bool },
    /// A `Future` that returned `Pending` is waiting to be woken up
    Sleeping,
    /// Task has finished
    Finished,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
pub(crate) struct ParkState {
    /// Whether the task's park token is curently available. If it's available, then the next time
    /// the task calls `park`, the token will be atomically consumed and the task will continue
    /// executing. If it's not available, then the task will block until either another task makes
    /// it available with `unpark`, or a spurious wakeup occurs.
    token_available: bool,

    /// Whether the task is currently blocked in a call to `park`.
    /// Invariant: `!(token_available && blocked_in_park)`. If the token is available, then the task
    /// shouldn't be blocked in a call to `park`---the task should either have been woken up when
    /// the token became available, or never have blocked in the first place if the token was
    /// available before the call to `park`.
    blocked_in_park: bool,
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

    /// Add a task to the set. If the set did not have this value present, `true` is returned. If
    /// the set did have this value present, `false` is returned.
    pub fn insert(&mut self, tid: TaskId) -> bool {
        if tid.0 >= self.tasks.len() {
            self.tasks.resize(1 + tid.0, false);
        }
        !std::mem::replace(&mut *self.tasks.get_mut(tid.0).unwrap(), true)
    }

    /// Removes a value from the set. Returns whether the value was present in the set.
    pub fn remove(&mut self, tid: TaskId) -> bool {
        if tid.0 >= self.tasks.len() {
            return false;
        }
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

impl<T: 'static> From<&'static LocalKey<T>> for StorageKey {
    fn from(key: &'static LocalKey<T>) -> Self {
        Self(key as *const _ as usize, 0x1)
    }
}
