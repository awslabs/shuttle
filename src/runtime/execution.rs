use crate::runtime::task::{PendingTask, Task, TaskId, TaskResult, TaskState, MAX_TASKS};
use crate::scheduler::Scheduler;
use scoped_tls::scoped_thread_local;
use smallvec::SmallVec;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::rc::Rc;

// We use this scoped TLS to smuggle the ExecutionState, which is not 'static, across continuations.
// The scoping handles the lifetime issue. Note that TLS is not aware of our generator threads; this
// is one shared cell across all threads in the execution.
scoped_thread_local! {
    static EXECUTION_STATE: RefCell<ExecutionState>
}

/// An execution is the top-level data structure for a single run of a function under test. It holds
/// the "state" of the execution (which tasks are blocked, finished, etc).
///
/// The "state" needs to be mutably accessible from each of the continuations. The Execution
/// arranges that whenever it resumes a continuation, the `EXECUTION_STATE` static holds a reference
/// to the current execution state.
pub(crate) struct Execution<'a> {
    state: RefCell<ExecutionState>,
    // can't make this a type parameter because we have static methods we want to call
    scheduler: &'a mut Box<dyn Scheduler>,
}

impl<'a> Execution<'a> {
    /// Construct a new execution that will use the given scheduler. The execution should then be
    /// invoked via its `run` method, which takes as input the closure for task 0.
    // TODO this is where we'd pass in config for max_tasks etc
    pub(crate) fn new(scheduler: &'a mut Box<dyn Scheduler>) -> Self {
        let state = RefCell::new(ExecutionState::new());
        Self { state, scheduler }
    }
}

impl Execution<'_> {
    /// Run a function to be tested, taking control of scheduling it and any tasks it might spawn.
    /// This function runs until `f` and all tasks spawned by `f` have terminated.
    pub(crate) fn run<F>(self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        // Install `f` as the first task to be spawned
        self.state.borrow_mut().enqueue_sync(f);

        loop {
            let mut state = self.state.borrow_mut();

            // Actually spawn any tasks that are ready to spawn. This leaves them in the Runnable state, but they have not been started.
            while let Some(pending_task) = state.pending.pop_front() {
                let task_id = TaskId(state.tasks.len());
                let task = Task::from_pending(task_id, pending_task);
                state.tasks.push(task);
            }

            let runnable = state
                .tasks
                .iter()
                .filter(|t| t.state == TaskState::Runnable)
                .map(|t| t.id)
                .collect::<SmallVec<[_; MAX_TASKS]>>();

            if runnable.is_empty() {
                let task_states = state
                    .tasks
                    .iter()
                    .map(|t| (t.id, t.state))
                    .collect::<SmallVec<[_; MAX_TASKS]>>();
                if task_states.iter().any(|(_, s)| *s == TaskState::Blocked) {
                    panic!("deadlock: {:?}", task_states);
                }
                assert!(state.tasks.iter().all(|t| t.finished()));
                break;
            }

            let to_run = self.scheduler.next_task(&runnable, state.current_task);
            let task = state.tasks.get(to_run.0).unwrap();

            // TODO Not sure if we should expose atomic tasks here; maybe wrap this all behind the
            // TODO Task abstraction?
            let atomic_task = Rc::clone(&task.inner);

            state.current_task = Some(to_run);
            drop(state);

            let ret = EXECUTION_STATE.set(&self.state, || atomic_task.borrow_mut().step());

            let mut state = self.state.borrow_mut();
            match ret {
                TaskResult::Finished => {
                    state.current_mut().state = TaskState::Finished;
                }
                TaskResult::Yielded => (),
                _ => panic!("unexpected continuation output"),
            }
        }

        // The program is now finished. These are just sanity checks for cleanup: every task should
        // have reached Finished state, and should be cleaned up (which does additional checks).
        let state = self.state.into_inner();
        for thd in state.tasks.into_iter() {
            assert_eq!(thd.state, TaskState::Finished);
            let task = Rc::try_unwrap(thd.inner);
            if let Ok(task) = task {
                let task = task.into_inner();
                task.cleanup();
            }
        }
    }

    /// Invoke a closure with access to the current execution state. Library code uses this to gain
    /// access to the state of the execution to influence scheduling (e.g. to register a task as
    /// blocked).
    pub(crate) fn with_state<F, T>(f: F) -> T
    where
        F: FnOnce(&mut ExecutionState) -> T,
    {
        EXECUTION_STATE.with(|cell| f(&mut *cell.borrow_mut()))
    }

    /// Spawn a new task. This doesn't create a yield point -- the caller should do that if
    /// appropriate.
    pub(crate) fn spawn<F>(f: F) -> TaskId
    where
        F: FnOnce() + Send + 'static,
    {
        Self::with_state(|state| state.enqueue_sync(f))
    }

    /// Spawn a new task. This doesn't create a yield point -- the caller should do that if
    /// appropriate.
    pub(crate) fn spawn_async(fut: impl Future<Output = ()> + 'static + Send) -> TaskId {
        Self::with_state(|state| state.enqueue_async(fut))
    }

    /// A shortcut to get the current task ID
    pub(crate) fn me() -> TaskId {
        Self::with_state(|s| s.current().id())
    }
}

/// `ExecutionState` contains the portion of a single execution's state that needs to be reachable
/// from within a continuation. It tracks which tasks exist and their states, as well as which
/// tasks are pending spawn.
pub(crate) struct ExecutionState {
    // invariant: tasks are never removed from this list, except during teardown of an `Execution`
    tasks: Vec<Task>,
    // invariant: if this transitions from Some -> None, it can never change again
    current_task: Option<TaskId>,
    pending: VecDeque<PendingTask>,
}

impl ExecutionState {
    fn new() -> Self {
        Self {
            tasks: vec![],
            current_task: None,
            pending: VecDeque::new(),
        }
    }

    fn next_task_id(&self) -> usize {
        self.tasks.len() + self.pending.len()
    }

    fn enqueue_async(&mut self, fut: impl Future<Output = ()> + 'static + Send) -> TaskId {
        let next_task_id = self.next_task_id();
        self.pending.push_back(PendingTask::AsyncTask(Box::pin(fut)));
        TaskId(next_task_id)
    }

    /// Spawn a new task.
    fn enqueue_sync<F>(&mut self, f: F) -> TaskId
    where
        F: FnOnce() + Send + 'static,
    {
        let next_task_id = self.next_task_id();
        self.pending.push_back(PendingTask::SyncTask(Box::new(f)));
        TaskId(next_task_id)
    }

    pub(crate) fn current(&self) -> &Task {
        self.get(self.current_task.unwrap())
    }

    pub(crate) fn current_mut(&mut self) -> &mut Task {
        self.get_mut(self.current_task.unwrap())
    }

    pub(crate) fn get(&self, id: TaskId) -> &Task {
        self.tasks.get(id.0).unwrap()
    }

    pub(crate) fn get_mut(&mut self, id: TaskId) -> &mut Task {
        self.tasks.get_mut(id.0).unwrap()
    }
}
