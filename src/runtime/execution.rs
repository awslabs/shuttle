use crate::runtime::metrics::MetricsScheduler;
use crate::runtime::task::{Task, TaskId, TaskState, MAX_TASKS};
use crate::runtime::thread_future::ThreadFuture;
use crate::scheduler::Scheduler;
use futures::future::BoxFuture;
use scoped_tls::scoped_thread_local;
use smallvec::SmallVec;
use std::any::Any;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::panic;
use std::rc::Rc;
use std::task::{Context, Poll};

// We use this scoped TLS to smuggle the ExecutionState, which is not 'static, across tasks that
// need access to it (to spawn new tasks, interrogate task status, etc).
scoped_thread_local! {
    static EXECUTION_STATE: RefCell<ExecutionState>
}

/// An `Execution` encapsulates a single run of a function under test against a chosen scheduler.
/// Its only useful method is `Execution::run`, which executes the function to completion. It also
/// has some static methods useful from library code.
///
/// The key thing that an `Execution` manages is the `ExecutionState`, which contains all the
/// mutable state a test's tasks might need access to during execution (to block/unblock tasks,
/// spawn new tasks, etc). The `Execution` makes this state available through the `EXECUTION_STATE`
/// static variable, but clients get access to it by calling `Execution::with_state`.
pub(crate) struct Execution<'a> {
    // can't make this a type parameter because we have static methods we want to call
    scheduler: MetricsScheduler<'a>,
}

impl<'a> Execution<'a> {
    /// Construct a new execution that will use the given scheduler. The execution should then be
    /// invoked via its `run` method, which takes as input the closure for task 0.
    // TODO this is where we'd pass in config for max_tasks etc
    pub(crate) fn new(scheduler: &'a mut Box<dyn Scheduler>) -> Self {
        let scheduler = MetricsScheduler::new(scheduler);
        Self { scheduler }
    }
}

impl Execution<'_> {
    /// Run a function to be tested, taking control of scheduling it and any tasks it might spawn.
    /// This function runs until `f` and all tasks spawned by `f` have terminated.
    pub(crate) fn run<F>(mut self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let state = RefCell::new(ExecutionState::new());

        EXECUTION_STATE.set(&state, || {
            // Install `f` as the first task to be spawned
            let first_task = ThreadFuture::new(f);
            Self::with_state(|state| {
                state.enqueue(first_task);
            });

            // Run the test to completion
            while self.step() {}
        });

        // The program is now finished. These are just sanity checks for cleanup: every task should
        // have reached Finished state, and should have no outstanding references to its Future.
        let state = state.into_inner();
        for thd in state.tasks.into_iter() {
            assert_eq!(thd.state, TaskState::Finished);
            Rc::try_unwrap(thd.future)
                .map_err(|_| ())
                .expect("couldn't cleanup a future");
        }
    }

    /// Execute a single step of the scheduler. Returns true if a step was taken or false if all
    /// tasks were finished.
    #[inline]
    fn step(&mut self) -> bool {
        let future = Self::with_state(|state| {
            // Actually spawn any tasks that are ready to spawn. This leaves them in the Runnable
            // state, but they have not been started.
            while let Some(future) = state.pending.pop_front() {
                let task_id = TaskId(state.tasks.len());
                let task = Task::new(future, task_id);
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
                    panic!(
                        "deadlock with schedule: {:?}\nrunnable tasks: {:?}",
                        self.scheduler.serialized_schedule(),
                        task_states,
                    );
                }
                assert!(state.tasks.iter().all(|t| t.finished()));
                return None;
            }

            let to_run = self.scheduler.next_task(&runnable, state.current_task);
            let task = state.tasks.get(to_run.0).unwrap();
            state.current_task = Some(to_run);

            Some(Rc::clone(&task.future))
        });

        // Run a single step of the chosen future
        let ret = match future {
            Some(future) => panic::catch_unwind(panic::AssertUnwindSafe(|| {
                // TODO implement real waker support
                let waker = futures::task::noop_waker_ref();
                let mut cx = &mut Context::from_waker(waker);
                future.borrow_mut().as_mut().poll(&mut cx)
            })),
            None => return false,
        };

        Self::with_state(|state| {
            match ret {
                Ok(Poll::Ready(_)) => {
                    state.current_mut().state = TaskState::Finished;
                }
                // TODO if it's a real future, we should mark it blocked here rather than polling it
                // TODO indefinitely, once we have waker support
                Ok(Poll::Pending) => (),
                Err(e) => {
                    let msg = format!(
                        "test panicked with schedule: {:?}",
                        self.scheduler.serialized_schedule()
                    );
                    eprintln!("{}", msg);
                    eprintln!("pass that schedule string into `shuttle::replay` to reproduce the failure");
                    // Try to inject the schedule into the panic payload if we can
                    let payload: Box<dyn Any + Send> = match e.downcast::<String>() {
                        Ok(panic_msg) => Box::new(format!("{}\noriginal panic: {}", msg, panic_msg)),
                        Err(panic) => panic,
                    };
                    panic::resume_unwind(payload);
                }
            }
        });

        true
    }

    /// Invoke a closure with access to the current execution state. Library code uses this to gain
    /// access to the state of the execution to influence scheduling (e.g. to register a task as
    /// blocked).
    #[inline]
    pub(crate) fn with_state<F, T>(f: F) -> T
    where
        F: FnOnce(&mut ExecutionState) -> T,
    {
        EXECUTION_STATE.with(|cell| f(&mut *cell.borrow_mut()))
    }

    /// Spawn a new future. This doesn't create a yield point -- the caller should do that if
    /// appropriate.
    pub(crate) fn spawn(fut: impl Future<Output = ()> + 'static + Send) -> TaskId {
        Self::with_state(|state| state.enqueue(fut))
    }

    /// A shortcut to get the current task ID
    pub(crate) fn me() -> TaskId {
        Self::with_state(|s| s.current().id())
    }
}

/// `ExecutionState` contains the portion of a single execution's state that needs to be reachable
/// from within a task's execution. It tracks which tasks exist and their states, as well as which
/// tasks are pending spawn.
pub(crate) struct ExecutionState {
    // invariant: tasks are never removed from this list
    tasks: Vec<Task>,
    // invariant: if this transitions from Some -> None, it can never change again
    current_task: Option<TaskId>,
    pending: VecDeque<BoxFuture<'static, ()>>,
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

    fn enqueue(&mut self, fut: impl Future<Output = ()> + 'static + Send) -> TaskId {
        let next_task_id = self.next_task_id();
        self.pending.push_back(Box::pin(fut));
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
