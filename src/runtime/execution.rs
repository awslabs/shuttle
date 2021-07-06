use crate::runtime::failure::{init_panic_hook, persist_failure, persist_task_failure};
use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::{Task, TaskId, TaskState, DEFAULT_INLINE_TASKS};
use crate::runtime::thread::continuation::PooledContinuation;
use crate::scheduler::{Schedule, Scheduler};
use crate::{Config, MaxSteps};
use futures::Future;
use scoped_tls::scoped_thread_local;
use smallvec::SmallVec;
use std::any::Any;
use std::cell::RefCell;
use std::panic;
use std::rc::Rc;
use tracing::span::Entered;
use tracing::{span, trace, Level, Span};

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

        let _guard = init_panic_hook(config.clone());

        EXECUTION_STATE.set(&state, move || {
            // Spawn `f` as the first task
            ExecutionState::spawn_thread(f, config.stack_size, None, Some(VectorClock::new()));

            // Run the test to completion
            while self.step(config) {}

            // Cleanup the state before it goes out of `EXECUTION_STATE` scope
            ExecutionState::cleanup();
        });
    }

    /// Execute a single step of the scheduler. Returns true if the execution should continue.
    #[inline]
    fn step(&mut self, config: &Config) -> bool {
        enum NextStep {
            Task(Rc<RefCell<PooledContinuation>>),
            Failure(String, Schedule),
            Finished,
        }

        let next_step = ExecutionState::with(|state| {
            if let Err(msg) = state.schedule() {
                return NextStep::Failure(msg, state.current_schedule.clone());
            }
            state.advance_to_next_task();

            match state.current_task {
                ScheduledTask::Some(tid) => {
                    let task = state.get(tid);
                    NextStep::Task(Rc::clone(&task.continuation))
                }
                ScheduledTask::Finished => {
                    let task_states = state
                        .tasks
                        .iter()
                        .map(|t| (t.id, t.state))
                        .collect::<SmallVec<[_; DEFAULT_INLINE_TASKS]>>();
                    if task_states.iter().any(|(_, s)| *s == TaskState::Blocked) {
                        NextStep::Failure(
                            format!("deadlock! runnable tasks: {:?}", task_states),
                            state.current_schedule.clone(),
                        )
                    } else {
                        debug_assert!(state.tasks.iter().all(|t| t.finished()));
                        NextStep::Finished
                    }
                }
                ScheduledTask::Stopped => NextStep::Finished,
                ScheduledTask::None => {
                    NextStep::Failure("no task was scheduled".to_string(), state.current_schedule.clone())
                }
            }
        });

        // Run a single step of the chosen task.
        let ret = match next_step {
            NextStep::Task(continuation) => {
                panic::catch_unwind(panic::AssertUnwindSafe(|| continuation.borrow_mut().resume()))
            }
            NextStep::Failure(msg, schedule) => {
                // Because we're creating the panic here, we don't need `persist_failure` to print
                // as the failure message will be part of the panic payload.
                let message = persist_failure(&schedule, msg, config, false);
                panic!("{}", message);
            }
            NextStep::Finished => return false,
        };

        match ret {
            // Task finished
            Ok(true) => {
                ExecutionState::with(|state| state.current_mut().finish());
            }
            // Task yielded
            Ok(false) => {}
            // Task failed
            Err(e) => {
                let (name, schedule) = ExecutionState::failure_info().unwrap();
                let message = persist_task_failure(&schedule, name, config, true);
                // Try to inject the schedule into the panic payload if we can
                let payload: Box<dyn Any + Send> = match e.downcast::<String>() {
                    Ok(panic_msg) => Box::new(format!("{}\noriginal panic: {}", message, panic_msg)),
                    Err(panic) => panic,
                };
                panic::resume_unwind(payload);
            }
        }

        true
    }
}

/// `ExecutionState` contains the portion of a single execution's state that needs to be reachable
/// from within a task's execution. It tracks which tasks exist and their states, as well as which
/// tasks are pending spawn.
pub(crate) struct ExecutionState {
    pub config: Config,
    // invariant: tasks are never removed from this list
    tasks: SmallVec<[Task; DEFAULT_INLINE_TASKS]>,
    // invariant: if this transitions to Stopped or Finished, it can never change again
    current_task: ScheduledTask,
    // the task the scheduler has chosen to run next
    next_task: ScheduledTask,
    // whether the current task has asked to yield
    has_yielded: bool,
    // the number of scheduling decisions made so far
    context_switches: usize,

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
        Self {
            config,
            tasks: SmallVec::new(),
            current_task: ScheduledTask::None,
            next_task: ScheduledTask::None,
            has_yielded: false,
            context_switches: 0,
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

    /// Like `with`, but returns None instead of panicing if there is no current ExecutionState or
    /// if the current ExecutionState is already borrowed.
    pub(crate) fn try_with<F, T>(f: F) -> Option<T>
    where
        F: FnOnce(&mut ExecutionState) -> T,
    {
        if EXECUTION_STATE.is_set() {
            EXECUTION_STATE.with(|cell| {
                if let Ok(mut state) = cell.try_borrow_mut() {
                    Some(f(&mut *state))
                } else {
                    None
                }
            })
        } else {
            None
        }
    }

    /// A shortcut to get the current task ID
    pub(crate) fn me() -> TaskId {
        Self::with(|s| s.current().id())
    }

    /// Spawn a new task for a future. This doesn't create a yield point; the caller should do that
    /// if it wants to give the new task a chance to run immediately.
    pub(crate) fn spawn_future<F>(future: F, stack_size: usize, name: Option<String>) -> TaskId
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Self::with(|state| {
            let task_id = TaskId(state.tasks.len());
            let clock = state.increment_clock_mut(); // Increment the parent's clock
            clock.extend(task_id); // and extend it with an entry for the new task
            let task = Task::from_future(future, stack_size, task_id, name, clock.clone());
            state.tasks.push(task);
            task_id
        })
    }

    pub(crate) fn spawn_thread<F>(
        f: F,
        stack_size: usize,
        name: Option<String>,
        mut initial_clock: Option<VectorClock>,
    ) -> TaskId
    where
        F: FnOnce() + Send + 'static,
    {
        Self::with(|state| {
            let task_id = TaskId(state.tasks.len());
            let clock = if let Some(ref mut clock) = initial_clock {
                clock
            } else {
                // Inherit the clock of the parent thread (which spawned this task)
                state.increment_clock_mut()
            };
            clock.extend(task_id); // and extend it with an entry for the new thread
            let task = Task::from_closure(f, stack_size, task_id, name, clock.clone());
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
            (std::mem::replace(&mut state.tasks, SmallVec::new()), state.current_task)
        });

        for task in tasks.drain(..) {
            assert!(
                final_state == ScheduledTask::Stopped || task.finished(),
                "execution finished but task is not"
            );
            Rc::try_unwrap(task.continuation)
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

            let result = state.schedule();
            // If scheduling failed, yield so that the outer scheduling loop can handle it.
            if result.is_err() {
                return true;
            }

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

    /// Tell the scheduler that the next context switch is an explicit yield requested by the
    /// current task. Some schedulers use this as a hint to influence scheduling.
    pub(crate) fn request_yield() {
        Self::with(|state| {
            state.has_yielded = true;
        });
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

    /// Generate some diagnostic information used when persisting failures.
    ///
    /// Because this method may be called from a panic hook, it must not panic.
    pub(crate) fn failure_info() -> Option<(String, Schedule)> {
        Self::try_with(|state| {
            let name = if let Some(task) = state.try_current() {
                task.name().unwrap_or_else(|| format!("task-{:?}", task.id().0))
            } else {
                "<unknown>".into()
            };
            (name, state.current_schedule.clone())
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
    pub(crate) fn try_current(&self) -> Option<&Task> {
        self.try_get(self.current_task.id()?)
    }

    pub(crate) fn get(&self, id: TaskId) -> &Task {
        self.try_get(id).unwrap()
    }

    pub(crate) fn get_mut(&mut self, id: TaskId) -> &mut Task {
        self.tasks.get_mut(id.0).unwrap()
    }

    pub(crate) fn try_get(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(id.0)
    }

    pub(crate) fn context_switches() -> usize {
        Self::with(|state| state.context_switches)
    }

    pub(crate) fn get_clock(&self, id: TaskId) -> &VectorClock {
        &self.tasks.get(id.0).unwrap().clock
    }

    pub(crate) fn get_clock_mut(&mut self, id: TaskId) -> &mut VectorClock {
        &mut self.tasks.get_mut(id.0).unwrap().clock
    }

    // Increment the current thread's clock entry and update its clock with the one provided.
    pub(crate) fn update_clock(&mut self, clock: &VectorClock) {
        let task = self.current_mut();
        task.clock.increment(task.id);
        task.clock.update(clock);
    }

    // Increment the current thread's clock and return a shared reference to it
    pub(crate) fn increment_clock(&mut self) -> &VectorClock {
        let task = self.current_mut();
        task.clock.increment(task.id);
        &task.clock
    }

    // Increment the current thread's clock and return a mutable reference to it
    pub(crate) fn increment_clock_mut(&mut self) -> &mut VectorClock {
        let task = self.current_mut();
        task.clock.increment(task.id);
        &mut task.clock
    }

    /// Run the scheduler to choose the next task to run. `has_yielded` should be false if the
    /// scheduler is being invoked from within a running task. If scheduling fails, returns an Err
    /// with a String describing the failure.
    fn schedule(&mut self) -> Result<(), String> {
        // Don't schedule twice. If `maybe_yield` ran the scheduler, we don't want to run it
        // again at the top of `step`.
        if self.next_task != ScheduledTask::None {
            return Ok(());
        }

        self.context_switches += 1;

        match self.config.max_steps {
            MaxSteps::FailAfter(n) if self.current_schedule.len() >= n => {
                let msg = format!(
                    "exceeded max_steps bound {}. this might be caused by an unfair schedule (e.g., a spin loop)?",
                    n
                );
                return Err(msg);
            }
            MaxSteps::ContinueAfter(n) if self.current_schedule.len() >= n => {
                self.next_task = ScheduledTask::Stopped;
                return Ok(());
            }
            _ => {}
        }

        let runnable = self
            .tasks
            .iter()
            .filter(|t| t.runnable())
            .map(|t| t.id)
            .collect::<SmallVec<[_; DEFAULT_INLINE_TASKS]>>();

        if runnable.is_empty() {
            self.next_task = ScheduledTask::Finished;
            return Ok(());
        }

        let is_yielding = std::mem::replace(&mut self.has_yielded, false);

        self.next_task = self
            .scheduler
            .borrow_mut()
            .next_task(&runnable, self.current_task.id(), is_yielding)
            .map(ScheduledTask::Some)
            .unwrap_or(ScheduledTask::Stopped);

        trace!(?runnable, next_task=?self.next_task);

        Ok(())
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
            self.current_span = span!(Level::INFO, "step", i = self.current_schedule.len() - 1, task = tid.0);
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
