use crate::runtime::failure::persist_failure;
use crate::runtime::task::{Task, TaskId, TaskState, TaskType, MAX_INLINE_TASKS};
use crate::runtime::thread::future::ThreadFuture;
use crate::scheduler::{Schedule, Scheduler};
use crate::Config;
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
    scheduler: Rc<RefCell<dyn Scheduler>>,
    initial_schedule: Schedule,
}

impl Execution {
    /// Construct a new execution that will use the given scheduler. The execution should then be
    /// invoked via its `run` method, which takes as input the closure for task 0.
    pub(crate) fn new(scheduler: Rc<RefCell<dyn Scheduler>>, initial_schedule: Schedule) -> Self {
        Self {
            scheduler,
            initial_schedule,
        }
    }
}

impl Execution {
    /// Run a function to be tested, taking control of scheduling it and any tasks it might spawn.
    /// This function runs until `f` and all tasks spawned by `f` have terminated, or until the
    /// scheduler returns `None`, indicating the execution should not be explored any further.
    pub(crate) fn run<F>(mut self, config: &Config, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let state = RefCell::new(ExecutionState::new(
            config.clone(),
            Rc::clone(&self.scheduler),
            self.initial_schedule.clone(),
        ));

        EXECUTION_STATE.set(&state, move || {
            // Spawn `f` as the first task
            let first_task = ThreadFuture::new(config.stack_size, f);
            ExecutionState::spawn(first_task, TaskType::Thread, None);

            // Run the test to completion
            while self.step(config) {}

            // Cleanup the state before it goes out of `EXECUTION_STATE` scope
            ExecutionState::cleanup();
        });
    }

    /// Execute a single step of the scheduler. Returns true if the execution should continue.
    #[inline]
    fn step(&mut self, config: &Config) -> bool {
        let result = ExecutionState::with(|state| {
            state.schedule();
            state.advance_to_next_task();

            match state.current_task {
                ScheduledTask::Some(tid) => {
                    let task = state.get(tid);
                    Some((Rc::clone(&task.future), task.waker()))
                }
                ScheduledTask::Finished => {
                    let task_states = state
                        .tasks
                        .iter()
                        .map(|t| (t.id, t.state))
                        .collect::<SmallVec<[_; MAX_INLINE_TASKS]>>();
                    if task_states.iter().any(|(_, s)| *s == TaskState::Blocked) {
                        panic!(persist_failure(
                            &state.current_schedule,
                            format!("deadlock! runnable tasks: {:?}", task_states),
                            config,
                        ));
                    }
                    debug_assert!(state.tasks.iter().all(|t| t.finished()));
                    None
                }
                ScheduledTask::Stopped => None,
                ScheduledTask::None => panic!("no task was scheduled"),
            }
        });

        // Run a single step of the chosen future. The future might be a ThreadFuture, in which case
        // it may run multiple steps of the same future if the scheduler so decides.
        let ret = match result {
            Some((future, waker)) => panic::catch_unwind(panic::AssertUnwindSafe(|| {
                let mut cx = &mut Context::from_waker(&waker);
                future.borrow_mut().as_mut().poll(&mut cx)
            })),
            None => return false,
        };

        ExecutionState::with(|state| {
            match ret {
                Ok(Poll::Ready(_)) => {
                    state.current_mut().state = TaskState::Finished;
                }
                Ok(Poll::Pending) => {
                    state.current_mut().block_after_running();
                }
                Err(e) => {
                    let name = if let ScheduledTask::Some(tid) = state.current_task {
                        state
                            .get(tid)
                            .name
                            .clone()
                            .unwrap_or_else(|| format!("task-{:?}", tid.0))
                    } else {
                        "?".into()
                    };
                    let msg = persist_failure(
                        &state.current_schedule,
                        format!("test panicked in task {:?}", name),
                        config,
                    );
                    eprintln!("{}", msg);
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
}

/// `ExecutionState` contains the portion of a single execution's state that needs to be reachable
/// from within a task's execution. It tracks which tasks exist and their states, as well as which
/// tasks are pending spawn.
pub(crate) struct ExecutionState {
    pub config: Config,
    // invariant: tasks are never removed from this list
    tasks: Vec<Task>,
    // invariant: if this transitions to Stopped or Finished, it can never change again
    current_task: ScheduledTask,
    // the task the scheduler has chosen to run next
    next_task: ScheduledTask,

    scheduler: Rc<RefCell<dyn Scheduler>>,
    current_schedule: Schedule,

    // For `tracing`, we track the current task's Span here and manage it in `schedule_next_task`.
    // Drop order is significant here; see the unsafe code in `schedule_next_task` for why.
    current_span_entered: Option<Entered<'static>>,
    current_span: Span,

    #[cfg(debug_assertions)]
    has_cleaned_up: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ScheduledTask {
    None,         // no task has ever been scheduled
    Some(TaskId), // this task is running
    Stopped,      // the scheduler asked us to stop running
    Finished,     // all tasks have finished running
}

impl ScheduledTask {
    fn id(&self) -> Option<TaskId> {
        match self {
            ScheduledTask::Some(tid) => Some(*tid),
            _ => None,
        }
    }

    fn take(&mut self) -> Self {
        std::mem::replace(self, ScheduledTask::None)
    }
}

impl ExecutionState {
    fn new(config: Config, scheduler: Rc<RefCell<dyn Scheduler>>, initial_schedule: Schedule) -> Self {
        let max_tasks = config.max_tasks;
        Self {
            config,
            tasks: Vec::with_capacity(max_tasks),
            current_task: ScheduledTask::None,
            next_task: ScheduledTask::None,
            scheduler,
            current_schedule: initial_schedule,
            current_span_entered: None,
            current_span: Span::none(),
            #[cfg(debug_assertions)]
            has_cleaned_up: false,
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
    pub(crate) fn spawn(
        future: impl Future<Output = ()> + 'static + Send,
        task_type: TaskType,
        name: Option<String>,
    ) -> TaskId {
        Self::with(|state| {
            let task_id = TaskId(state.tasks.len());
            let task = Task::new(Box::pin(future), task_id, task_type, name);
            state.tasks.push(task);
            task_id
        })
    }

    /// Prepare this ExecutionState to be dropped. Call this before dropping so that the tasks have
    /// a chance to run their drop handlers while `EXECUTION_STATE` is still in scope.
    fn cleanup() {
        // A slightly delicate dance here: we need to drop the tasks from outside of `Self::with`,
        // because a task's Drop impl might want to call back into `ExecutionState` (to check
        // `should_stop()`). So we pull the tasks out of the `ExecutionState`, leaving it in an
        // invalid state, but no one should still be accessing the tasks anyway.
        let (mut tasks, final_state) = Self::with(|state| {
            assert!(state.current_task == ScheduledTask::Stopped || state.current_task == ScheduledTask::Finished);
            (std::mem::replace(&mut state.tasks, Vec::new()), state.current_task)
        });

        for task in tasks.drain(..) {
            assert!(
                final_state == ScheduledTask::Stopped || task.finished(),
                "execution finished but task is not"
            );
            Rc::try_unwrap(task.future)
                .map_err(|_| ())
                .expect("couldn't cleanup a future");
        }

        #[cfg(debug_assertions)]
        Self::with(|state| state.has_cleaned_up = true);
    }

    /// Invoke the scheduler to decide which task to schedule next. Returns true if the chosen task
    /// is different from the currently running task, indicating that the current task should yield
    /// its execution.
    pub(crate) fn maybe_yield() -> bool {
        Self::with(|state| {
            debug_assert!(
                matches!(state.current_task, ScheduledTask::Some(_)) && state.next_task == ScheduledTask::None,
                "we're inside a task and scheduler should not yet have run"
            );

            state.schedule();

            // If the next task is the same as the current one, we can skip the context switch
            // and just advance to the next task immediately.
            if state.current_task == state.next_task {
                state.advance_to_next_task();
                false
            } else {
                true
            }
        })
    }

    /// Check whether the current execution has stopped. Call from `Drop` handlers to early exit if
    /// they are being invoked because an execution has stopped.
    ///
    /// We also stop if we are currently panicking (e.g., perhaps we're unwinding the stack for a
    /// panic triggered while someone held a Mutex, and so are executing the Drop handler for
    /// MutexGuard). This avoids calling back into the scheduler during a panic, because the state
    /// may be poisoned or otherwise invalid.
    pub(crate) fn should_stop() -> bool {
        std::thread::panicking()
            || Self::with(|s| {
                assert_ne!(s.current_task, ScheduledTask::Finished);
                s.current_task == ScheduledTask::Stopped
            })
    }

    /// Generate a random u64 from the current scheduler and return it.
    #[inline]
    pub(crate) fn next_u64() -> u64 {
        Self::with(|state| {
            state.current_schedule.push_random();
            state.scheduler.borrow_mut().next_u64()
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

    /// Run the scheduler to choose the next task to run
    fn schedule(&mut self) {
        // Don't schedule twice. If `maybe_yield` ran the scheduler, we don't want to run it
        // again at the top of `step`.
        if self.next_task != ScheduledTask::None {
            return;
        }

        let runnable = self
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Runnable)
            .map(|t| t.id)
            .collect::<Vec<_>>();

        if runnable.is_empty() {
            self.next_task = ScheduledTask::Finished;
            return;
        }

        self.next_task = self
            .scheduler
            .borrow_mut()
            .next_task(&runnable, self.current_task.id())
            .map(ScheduledTask::Some)
            .unwrap_or(ScheduledTask::Stopped);

        debug!(?runnable, next_task=?self.next_task);
    }

    /// Set the next task as the current task, and update our tracing span
    fn advance_to_next_task(&mut self) {
        debug_assert_ne!(self.next_task, ScheduledTask::None);
        self.current_task = self.next_task.take();
        if let ScheduledTask::Some(tid) = self.current_task {
            self.current_schedule.push_task(tid);
        }

        // Safety: Unfortunately `ExecutionState` is a static, but `Entered<'a>` is tied to the
        // lifetime 'a of its corresponding Span, so we can't stash the `Entered` into
        // `self.current_span_entered` directly. Instead, we transmute `Entered<'a>` into
        // `Entered<'static>`. We make sure that it can never outlive 'a by dropping
        // `self.current_span_entered` before dropping the `self.current_span` it points to.
        self.current_span_entered.take();
        if let ScheduledTask::Some(tid) = self.current_task {
            self.current_span = span!(Level::DEBUG, "step", i = self.current_schedule.len() - 1, task = tid.0);
            self.current_span_entered = Some(unsafe { extend_span_entered_lt(self.current_span.enter()) });
        }
    }
}

// Safety: see the use in `advance_to_next_task` above. We lift this out of that function so we can
// give fixed concrete types for the transmute.
unsafe fn extend_span_entered_lt<'a>(entered: Entered<'a>) -> Entered<'static> {
    std::mem::transmute(entered)
}

#[cfg(debug_assertions)]
impl Drop for ExecutionState {
    fn drop(&mut self) {
        assert!(self.has_cleaned_up || std::thread::panicking());
    }
}
