use crate::runtime::task::{Task, TaskId, TaskState, MAX_TASKS};
use crate::runtime::thread_future::ThreadFuture;
use crate::scheduler::metrics::MetricsScheduler;
use crate::scheduler::Scheduler;
use scoped_tls::scoped_thread_local;
use smallvec::SmallVec;
use std::any::Any;
use std::cell::RefCell;
use std::future::Future;
use std::panic;
use std::rc::Rc;
use std::task::{Context, Poll};
use tracing::span::Entered;
use tracing::{debug, span, Level, Span};

// We use this scoped TLS to smuggle the ExecutionState, which is not 'static, across tasks that
// need access to it (to spawn new tasks, interrogate task status, etc).
scoped_thread_local! {
    static EXECUTION_STATE: RefCell<ExecutionState>
}

/// An `Execution` encapsulates a single run of a function under test against a chosen scheduler.
/// Its only useful method is `Execution::run`, which executes the function to completion.
///
/// The key thing that an `Execution` manages is the `ExecutionState`, which contains all the
/// mutable state a test's tasks might need access to during execution (to block/unblock tasks,
/// spawn new tasks, etc). The `Execution` makes this state available through the `EXECUTION_STATE`
/// static variable, but clients get access to it by calling `ExecutionState::with`.
pub(crate) struct Execution {
    scheduler: Rc<RefCell<Box<dyn Scheduler>>>,
}

impl Execution {
    /// Construct a new execution that will use the given scheduler. The execution should then be
    /// invoked via its `run` method, which takes as input the closure for task 0.
    // TODO this is where we'd pass in config for max_tasks etc
    pub(crate) fn new(scheduler: Rc<RefCell<Box<dyn Scheduler>>>) -> Self {
        Self { scheduler }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StepResult {
    Continue, // execution is not done, keep exploring
    Finish,   // execution is done, all tasks should be finished
    Stop,     // execution is not done, but stop exploring
}

impl Execution {
    /// Run a function to be tested, taking control of scheduling it and any tasks it might spawn.
    /// This function runs until `f` and all tasks spawned by `f` have terminated, or until the
    /// scheduler returns `None`, indicating the execution should not be explored any further.
    pub(crate) fn run<F>(mut self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let scheduler = MetricsScheduler::new(Rc::clone(&self.scheduler));
        let state = RefCell::new(ExecutionState::new(scheduler));

        let result = EXECUTION_STATE.set(&state, || {
            // Spawn `f` as the first task
            let first_task = ThreadFuture::new(f);
            ExecutionState::spawn(first_task);

            // Run the test to completion
            loop {
                let result = self.step();
                if result != StepResult::Continue {
                    break result;
                }
            }
        });

        if result == StepResult::Finish {
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
    }

    /// Execute a single step of the scheduler. Returns true if a step was taken or false if all
    /// tasks were finished.
    #[inline]
    fn step(&mut self) -> StepResult {
        let result = ExecutionState::with(|state| {
            state.schedule_next_task();

            match state.current_task {
                CurrentTask::ToRun(tid) => {
                    state.current_task.mark_run();
                    (StepResult::Continue, Some(Rc::clone(&state.get(tid).future)))
                }
                CurrentTask::Finished => {
                    let task_states = state
                        .tasks
                        .iter()
                        .map(|t| (t.id, t.state))
                        .collect::<SmallVec<[_; MAX_TASKS]>>();
                    if task_states.iter().any(|(_, s)| *s == TaskState::Blocked) {
                        panic!(
                            "deadlock with schedule: {:?}\nrunnable tasks: {:?}",
                            state.scheduler.serialized_schedule(),
                            task_states,
                        );
                    }
                    assert!(state.tasks.iter().all(|t| t.finished()));
                    (StepResult::Finish, None)
                }
                CurrentTask::Stopped => (StepResult::Stop, None),
                _ => panic!("unexpected current_task {:?}", state.current_task),
            }
        });

        // Run a single step of the chosen future. The future might be a ThreadFuture, in which case
        // it may run multiple steps of the same future if the scheduler so decides.
        let ret = match result {
            (StepResult::Continue, Some(future)) => panic::catch_unwind(panic::AssertUnwindSafe(|| {
                // TODO implement real waker support
                let waker = futures::task::noop_waker_ref();
                let mut cx = &mut Context::from_waker(waker);
                future.borrow_mut().as_mut().poll(&mut cx)
            })),
            (step_result, _) => return step_result,
        };

        ExecutionState::with(|state| {
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
                        state.scheduler.serialized_schedule()
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

        StepResult::Continue
    }
}

/// `ExecutionState` contains the portion of a single execution's state that needs to be reachable
/// from within a task's execution. It tracks which tasks exist and their states, as well as which
/// tasks are pending spawn.
pub(crate) struct ExecutionState {
    // invariant: tasks are never removed from this list
    tasks: Vec<Task>,
    // invariant: if this transitions to Stopped or Finished, it can never change again
    current_task: CurrentTask,

    scheduler: MetricsScheduler,

    // For `tracing`, we track the current task's Span here and manage it in `schedule_next_task`.
    // Drop order is significant here; see the unsafe code in `schedule_next_task` for why.
    current_span_entered: Option<Entered<'static>>,
    current_span: Span,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum CurrentTask {
    Unscheduled,    // no task has ever been scheduled
    ToRun(TaskId),  // a task has been scheduled but not yet run
    HasRun(TaskId), // a task has been scheduled and has been run
    Stopped,        // the scheduler asked us to stop running
    Finished,       // all tasks have finished running
}

impl CurrentTask {
    fn id(&self) -> Option<TaskId> {
        match self {
            CurrentTask::ToRun(tid) => Some(*tid),
            CurrentTask::HasRun(tid) => Some(*tid),
            _ => None,
        }
    }

    fn mark_run(&mut self) {
        match self {
            CurrentTask::ToRun(tid) => *self = CurrentTask::HasRun(*tid),
            _ => panic!("cannot mark current task {:?} as run", self),
        }
    }
}

impl ExecutionState {
    fn new(scheduler: MetricsScheduler) -> Self {
        Self {
            tasks: vec![],
            current_task: CurrentTask::Unscheduled,
            scheduler,
            current_span_entered: None,
            current_span: Span::none(),
        }
    }

    /// Invoke a closure with access to the current execution state. Library code uses this to gain
    /// access to the state of the execution to influence scheduling (e.g. to register a task as
    /// blocked).
    #[inline]
    pub(crate) fn with<F, T>(f: F) -> T
    where
        F: FnOnce(&mut ExecutionState) -> T,
    {
        EXECUTION_STATE.with(|cell| f(&mut *cell.borrow_mut()))
    }

    /// A shortcut to get the current task ID
    pub(crate) fn me() -> TaskId {
        Self::with(|s| s.current().id())
    }

    /// Spawn a new task for a future. This doesn't create a yield point; the caller should do that
    /// if it wants to give the new task a chance to run immediately.
    pub(crate) fn spawn(future: impl Future<Output = ()> + 'static + Send) -> TaskId {
        Self::with(|state| {
            let task_id = TaskId(state.tasks.len());
            let task = Task::new(Box::pin(future), task_id);
            state.tasks.push(task);
            task_id
        })
    }

    /// Invoke the scheduler to decide which task to schedule next. Returns true if the chosen task
    /// is different from the currently running task, indicating that the current task should yield
    /// its execution.
    pub(crate) fn maybe_yield() -> bool {
        Self::with(|s| {
            if s.schedule_next_task() {
                true
            } else {
                // We're running the task again immediately
                s.current_task.mark_run();
                false
            }
        })
    }

    pub(crate) fn current(&self) -> &Task {
        self.get(self.current_task.id().unwrap())
    }

    pub(crate) fn current_mut(&mut self) -> &mut Task {
        self.get_mut(self.current_task.id().unwrap())
    }

    pub(crate) fn get(&self, id: TaskId) -> &Task {
        self.tasks.get(id.0).unwrap()
    }

    pub(crate) fn get_mut(&mut self, id: TaskId) -> &mut Task {
        self.tasks.get_mut(id.0).unwrap()
    }

    /// Run the scheduler to choose the next task to run. Returns true if the current task changes.
    fn schedule_next_task(&mut self) -> bool {
        let current = self.current_task;

        // Don't schedule twice if we've already got a pending task to run, or if it's futile
        if matches!(current, CurrentTask::ToRun(_) | CurrentTask::Stopped | CurrentTask::Finished) {
            return false;
        }

        let runnable = self
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Runnable)
            .map(|t| t.id)
            .collect::<SmallVec<[_; MAX_TASKS]>>();

        // No more runnable tasks, so we are finished. Don't invoke the scheduler again.
        if runnable.is_empty() {
            self.current_task = CurrentTask::Finished;
            return true;
        }

        self.current_task = self
            .scheduler
            .next_task(&runnable, self.current_task.id())
            .map(CurrentTask::ToRun)
            .unwrap_or(CurrentTask::Stopped);

        self.current_span_entered.take();

        debug!(?runnable, to_run=?self.current_task);

        // Scheduler asked us to stop early. Clean up the Span before we leave.
        if self.current_task == CurrentTask::Stopped {
            self.current_span = Span::none();
            return true;
        }

        // Safety: Unfortunately `ExecutionState` is a static, but `Entered<'a>` is tied to the
        // lifetime 'a of its corresponding Span, so we can't stash the `Entered` into
        // `self.current_span_entered` directly. Instead, we transmute `Entered<'a>` into
        // `Entered<'static>`. We make sure that it can never outlive 'a by dropping
        // `self.current_span_entered` above before dropping the `self.current_span` it points to.
        self.current_span = span!(
            Level::DEBUG,
            "step",
            i = self.scheduler.current_schedule().len() - 1,
            task_id = self.current_task.id().unwrap().0
        );
        self.current_span_entered = Some(unsafe { extend_span_entered_lt(self.current_span.enter()) });

        current != self.current_task
    }
}

// Safety: see the use in `schedule_next_task` above. We lift this out of that function so we can
// give fixed concrete types for the transmute.
unsafe fn extend_span_entered_lt<'a>(entered: Entered<'a>) -> Entered<'static> {
    std::mem::transmute(entered)
}
