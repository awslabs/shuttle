use crate::runtime::failure::{init_panic_hook, persist_failure};
use crate::runtime::storage::{StorageKey, StorageMap};
use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::labels::Labels;
use crate::runtime::task::{ChildLabelFn, Task, TaskId, TaskName, TaskSignature, DEFAULT_INLINE_TASKS};
use crate::runtime::thread;
use crate::runtime::thread::continuation::PooledContinuation;
use crate::scheduler::{Schedule, Scheduler};
use crate::sync::{ResourceSignature, ResourceType};
use crate::thread::thread_fn;
use crate::{backtrace_enabled, Config, MaxSteps, UNGRACEFUL_SHUTDOWN_CONFIG};
use scoped_tls::scoped_thread_local;
use smallvec::SmallVec;
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Debug;
use std::future::Future;
use std::panic::{self, Location};
use std::rc::Rc;
use std::sync::Arc;
use tracing::{trace, Span};

#[allow(deprecated)]
use super::task::Tag;

// We use this scoped TLS to smuggle the ExecutionState, which is not 'static, across tasks that
// need access to it (to spawn new tasks, interrogate task status, etc).
scoped_thread_local! {
    static EXECUTION_STATE: RefCell<ExecutionState>
}

// The reason this is separated out from `ExecutionState` is to ensure that we're always able to persist the schedule.
// If we don't do this, then we may panic while borrowing `ExecutionState`, and then not be able to emit the schedule.
// If we then panic again while trying to handle the panic, such that the panic becomes an abort, we will never log
// the schedule.
//
// It is expected that if the `ExecutionState` exists, then this will exist, and any usage of this happens through the
// `ExecutionState`, or at a point where it is known that the `ExecutionState` must exist (eg. when serializing on a panic).
thread_local! {
    static CURRENT_SCHEDULE: CurrentSchedule = CurrentSchedule::default();
}

#[derive(Debug, Default)]
pub struct CurrentSchedule {
    current_schedule: RefCell<Schedule>,
}

impl CurrentSchedule {
    fn init(schedule: Schedule) {
        CURRENT_SCHEDULE.with(|cs| *cs.current_schedule.borrow_mut() = schedule)
    }

    /// Add the given task ID as the next step of the schedule.
    fn push_task(tid: TaskId) {
        CURRENT_SCHEDULE.with(|cs| cs.current_schedule.borrow_mut().push_task(tid))
    }

    /// Add a choice of a random u64 value as the next step of the schedule
    fn push_random() {
        CURRENT_SCHEDULE.with(|cs| cs.current_schedule.borrow_mut().push_random())
    }

    /// Return the number of steps in the schedule
    pub(crate) fn len() -> usize {
        CURRENT_SCHEDULE.with(|cs| (*cs.current_schedule.borrow()).len())
    }

    /// Returns a clone of the inner schedule
    pub(crate) fn get_schedule() -> Schedule {
        CURRENT_SCHEDULE.with(|cs| (*cs.current_schedule.borrow()).clone())
    }
}

thread_local! {
    #[allow(clippy::complexity)]
    #[allow(deprecated)]
    pub(crate) static TASK_ID_TO_TAGS: RefCell<HashMap<TaskId, Arc<dyn Tag>>> = RefCell::new(HashMap::new());
}

thread_local! {
    pub(crate) static LABELS: RefCell<HashMap<TaskId, Labels>> = RefCell::new(HashMap::new());
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

#[derive(Debug)]
enum StepError {
    // Contains the panic payload of the task that failed.
    TaskFailure(Box<dyn Any + Send>),
    // The scheduler didn't make a decision. Indicates a scheduler error.
    SchedulingError,
    // Scheduling deetected a deadlock.
    Deadlock,
    // We exceeded the step bound.
    StepBoundExceeded,
    // Task panic and `config.immediately_return_on_panic` is set to `true`.
    TaskPanicEarlyReturn,
}

impl StepError {
    fn persist_failure(&self, config: &Config) {
        if let StepError::StepBoundExceeded = self {
            if let MaxSteps::ContinueAfter(_) = config.max_steps {
                return;
            }
        }
        persist_failure(config);
    }
}

impl Execution {
    /// Run a function to be tested, taking control of scheduling it and any tasks it might spawn.
    /// This function runs until `f` and all tasks spawned by `f` have terminated, or until the
    /// scheduler returns `None`, indicating the execution should not be explored any further.
    pub(crate) fn run<F>(mut self, config: &Config, f: F, caller: &'static Location<'static>)
    where
        F: FnOnce() + Send + 'static,
    {
        let state = RefCell::new(ExecutionState::new(config.clone(), Rc::clone(&self.scheduler)));

        init_panic_hook(config.clone());
        CurrentSchedule::init(self.initial_schedule.clone());
        UNGRACEFUL_SHUTDOWN_CONFIG.set(config.ungraceful_shutdown_config);

        EXECUTION_STATE.set(&state, move || {
            // Spawn `f` as the first task
            ExecutionState::spawn_main_thread(
                Box::new(move || thread_fn(f, true, Default::default())),
                config.stack_size,
                caller,
            );

                // Run the test to completion
                match self.run_to_competion(UNGRACEFUL_SHUTDOWN_CONFIG.get().immediately_return_on_panic) {
                    Ok(()) => {},
                    Err(e) => {
                        e.persist_failure(config);

                        match e {
                            StepError::TaskFailure(payload) => {
                                eprintln!("test panicked in task '{}'", ExecutionState::failing_task());

                                panic::resume_unwind(payload);
                            }
                            StepError::Deadlock => {
                                let blocked_tasks = ExecutionState::with(|state|
                                    state
                                    .tasks
                                    .iter()
                                    .filter(|t| !t.finished())
                                    .map(|t| t.format_for_deadlock())
                                    .collect::<Vec<_>>());

                                // Collecting backtraces is expensive, so we only want to do it if the user opts in to collecting them.
                                if !backtrace_enabled() {
                                    eprintln!("Test deadlocked, and {} is not set. If either of those are set then the backtrace of each task will be collected and printed as part of the panic message.", crate::CAPTURE_BACKTRACE)
                                }

                                panic!("deadlock! blocked tasks: [{}]", blocked_tasks.join(", "));
                            }
                            StepError::SchedulingError => panic!("no task was scheduled\nThis indicates an issue with the scheduler."),
                            StepError::StepBoundExceeded => {
                                if let MaxSteps::FailAfter(max_steps) = config.max_steps {
                                    panic!("exceeded max_steps bound {max_steps}. this might be caused by an unfair schedule (e.g., a spin loop)?");
                                }
                            }
                            StepError::TaskPanicEarlyReturn => panic::resume_unwind(Box::new("Task panicked, and early return is enabled.")),
                        }
                    }}


                // Cleanup the state before it goes out of `EXECUTION_STATE` scope
                ExecutionState::cleanup();
            });
    }

    fn enter_task_span() {
        // Enter the Task's span
        // (Note that if any issues arise with spans and tracing, then
        // 1) calling `exit` until `None` before entering the `Task`s `Span`,
        // 2) storing the entirety of the `span_stack` when creating the `Task`, and
        // 3) storing `top_level_span` as a stack
        // should be tried.)
        ExecutionState::with(|state| {
            tracing::dispatcher::get_default(|subscriber| {
                if let Some(span_id) = tracing::Span::current().id().as_ref() {
                    subscriber.exit(span_id);
                }

                // The `span_stack` stores `Span`s such that the top of the stack is the outermost `Span`,
                // meaning that parents (left-most when printed) are entered first.
                while let Some(span) = state.current_mut().span_stack.pop() {
                    if let Some(span_id) = span.id().as_ref() {
                        subscriber.enter(span_id)
                    }
                }

                if state.config.record_steps_in_span {
                    state.current().step_span.record("i", CurrentSchedule::len());
                }
            });
        });
    }

    fn exit_task_span() {
        // Leave the Task's span and store the exited `Span` stack in order to restore it the next time the Task is run
        ExecutionState::with(|state| {
            tracing::dispatcher::get_default(|subscriber| {
                debug_assert!(state.current().span_stack.is_empty());
                while let Some(span_id) = tracing::Span::current().id().as_ref() {
                    state.current_mut().span_stack.push(tracing::Span::current().clone());
                    subscriber.exit(span_id);
                }

                if let Some(span_id) = state.top_level_span.id().as_ref() {
                    subscriber.enter(span_id)
                }
            });
        });
    }

    /// Run the execution to completion.
    #[inline]
    fn run_to_competion(&mut self, immediately_return_on_panic: bool) -> Result<(), StepError> {
        loop {
            let next_step: Option<Rc<RefCell<PooledContinuation>>> = ExecutionState::with(|state| {
                state.schedule()?;
                state.advance_to_next_task();

                match state.current_task {
                    ScheduledTask::Some(tid) => {
                        let task = state.get(tid);
                        Ok(Some(task.continuation.clone()))
                    }
                    ScheduledTask::Finished => {
                        // The scheduler decided we're finished, so there are either no runnable tasks,
                        // or all runnable tasks are detached and there are no unfinished attached
                        // tasks. Therefore, it's a deadlock if there are unfinished attached tasks.
                        if state.tasks.iter().any(|t| !t.finished() && !t.detached) {
                            Err(StepError::Deadlock)
                        } else {
                            Ok(None)
                        }
                    }
                    ScheduledTask::Stopped => Ok(None),
                    ScheduledTask::None => Err(StepError::SchedulingError),
                }
            })?;

            // Run a single step of the chosen task.
            let ret = match next_step {
                Some(continuation) => {
                    Execution::enter_task_span();

                    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| continuation.borrow_mut().resume()));

                    Execution::exit_task_span();

                    result
                }
                None => return Ok(()),
            };

            match ret {
                // Task finished
                Ok(true) => {
                    crate::annotations::record_task_terminated();
                    ExecutionState::with(|state| state.current_mut().finish());
                }
                // Task yielded
                Ok(false) => {
                    // We may have `switch`ed out of the task before we finished unwinding the stack (ie. a `drop` handler calls `switch`).
                    // If `immediately_return_on_panic` is set, we will then return. If we don't do this, then we run the risk of panicking
                    // again in some other task, which would result in the test aborting.
                    if immediately_return_on_panic && std::thread::panicking() {
                        ExecutionState::with(|state| state.current_task = ScheduledTask::Stopped);
                        return Err(StepError::TaskPanicEarlyReturn);
                    }
                }
                // Task failed
                Err(e) => return Err(StepError::TaskFailure(e)),
            }
        }
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
    // the schedule length last time `reset_stop_bound()` was called
    pub(crate) steps_reset_at: usize,

    // static values for the current execution
    storage: StorageMap,

    scheduler: Rc<RefCell<dyn Scheduler>>,

    in_cleanup: bool,

    #[cfg(debug_assertions)]
    has_cleaned_up: bool,

    // The `Span` which the `ExecutionState` was created under. Will be the parent of all `Task` `Span`s
    pub(crate) top_level_span: Span,

    // Persistent Vec used as a bump allocator for references to runnable tasks to avoid slow allocation
    // on each scheduling decision. Should not be used outside of the `schedule` function
    runnable_tasks: Vec<*const Task>,
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

/// Error type for when an `ExecutionState::with` fails
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum ExecutionStateBorrowError {
    /// `ExecutionState` is currently not set
    NotSet,
    /// We are trying to borrow `ExecutionState` while it is already borrowed
    AlreadyBorrowed,
}

impl ExecutionState {
    fn new(config: Config, scheduler: Rc<RefCell<dyn Scheduler>>) -> Self {
        Self {
            config,
            tasks: SmallVec::new(),
            current_task: ScheduledTask::None,
            next_task: ScheduledTask::None,
            has_yielded: false,
            context_switches: 0,
            steps_reset_at: 0,
            storage: StorageMap::new(),
            scheduler,
            in_cleanup: false,
            #[cfg(debug_assertions)]
            has_cleaned_up: false,
            top_level_span: tracing::Span::current(),
            runnable_tasks: Vec::with_capacity(DEFAULT_INLINE_TASKS),
        }
    }

    /// Invoke a closure with access to the current execution state. Library code uses this to gain
    /// access to the state of the execution to influence scheduling (e.g. to register a task as
    /// blocked).
    #[inline]
    #[track_caller]
    pub(crate) fn with<F, T>(f: F) -> T
    where
        F: FnOnce(&mut ExecutionState) -> T,
    {
        Self::try_with(f).unwrap_or_else(|e| {
            eprintln!("`ExecutionState::try_with` failed with error: {e:?}");
            eprintln!(
                "Backtrace for `with`: {:#?}",
                std::backtrace::Backtrace::force_capture()
            );
            match e {
                ExecutionStateBorrowError::AlreadyBorrowed => panic!("`ExecutionState::with` panicked because `ExecutionState` is already borrowed."),
                ExecutionStateBorrowError::NotSet => panic!("`ExecutionState::with` panicked because `ExecutionState` is not set. Are you accessing a Shuttle primitive outside of a Shuttle test?"),
            }
        })
    }

    /// Like `with`, but returns None instead of panicking if there is no current ExecutionState or
    /// if the current ExecutionState is already borrowed.
    #[inline]
    #[track_caller]
    pub(crate) fn try_with<F, T>(f: F) -> Result<T, ExecutionStateBorrowError>
    where
        F: FnOnce(&mut ExecutionState) -> T,
    {
        trace!(
            "ExecutionState::try_with called from {:?}",
            std::panic::Location::caller()
        );
        if EXECUTION_STATE.is_set() {
            EXECUTION_STATE.with(|cell| {
                if let Ok(mut state) = cell.try_borrow_mut() {
                    Ok(f(&mut state))
                } else {
                    Err(ExecutionStateBorrowError::AlreadyBorrowed)
                }
            })
        } else {
            Err(ExecutionStateBorrowError::NotSet)
        }
    }

    /// A shortcut to get the current task ID
    pub(crate) fn me() -> TaskId {
        Self::with(|s| s.current().id())
    }

    /// If there is only one attached, unfinished task and there is at least one detached, unfinished task
    /// then exiting the attached task will cause the whole execution to exit. As a result, the unfinished
    /// detached tasks are truncated -- their remaining events will not be executed because the program itself
    /// has exited. This is relevant because it means that *exiting* a task can be a visible operation
    /// in that it affects which events are executed.
    pub(crate) fn exit_current_truncates_execution(&self) -> bool {
        // Strictly speaking, this is only true if there are other runnable detached tasks, but always making the main thread
        // exit a scheduling point is simpler conceptually
        if self.current().id() == TaskId::from(0) {
            return true;
        }

        // If the current task is detached, then it definitely doesn't truncate the execution
        if self.current().is_detached() {
            return false;
        }

        let mut single_unfinished_attached = false;
        let mut has_unfinished_detached = false;
        for t in self.tasks.iter() {
            let unfinished_attached = !t.finished() && !t.detached;
            if single_unfinished_attached && unfinished_attached {
                // there are more than one unfinished attached tasks, so one exiting won't truncate
                return false;
            }

            single_unfinished_attached |= unfinished_attached;
            has_unfinished_detached |= !t.finished() && t.detached;
        }
        has_unfinished_detached && single_unfinished_attached
    }

    fn set_labels_for_new_task(state: &ExecutionState, task_id: TaskId, name: Option<String>) {
        LABELS.with(|cell| {
            let mut map = cell.borrow_mut();

            // If parent has labels, inherit them
            if let Some(parent_task_id) = state.try_current().map(|t| t.id()) {
                let parent_map = map.get(&parent_task_id);
                if let Some(parent_map) = parent_map {
                    let mut child_map = parent_map.clone();

                    // If the parent has a `ChildLabelFn` set, use that to update the child's Labels
                    if let Some(gen) = parent_map.get::<ChildLabelFn>() {
                        (gen.0)(task_id, &mut child_map);
                    }

                    map.insert(task_id, child_map);
                }
            }

            // Add any name assigned to the task to its set of Labels
            if let Some(name) = name {
                let m = map.entry(task_id).or_default();
                m.insert(TaskName::from(name));
            }
        });
    }

    // Note: `spawn_thread`, `spawn_main_thread`, and `spawn_future` share some similar logic.
    // Changes to one of these functions likely need to be propagated to the other two as well.
    pub(crate) fn spawn_main_thread(
        f: Box<dyn FnOnce() + 'static>,
        stack_size: usize,
        caller: &'static Location<'static>,
    ) -> TaskId {
        let name = "main-thread".to_string();
        let mut clock = VectorClock::new();

        let task_id = Self::with(|state| {
            let parent_span_id = state.top_level_span.id();
            let task_id = TaskId(state.tasks.len());
            let tag = state.get_tag_or_default_for_current_task();

            Self::set_labels_for_new_task(state, task_id, Some(name.clone()));

            clock.extend(task_id); // and extend it with an entry for the new thread

            let schedule_len = CurrentSchedule::len();

            let task = Task::from_closure(
                f,
                stack_size,
                task_id,
                Some(name),
                clock,
                parent_span_id,
                schedule_len,
                tag,
                None,
                TaskSignature::new_parentless(caller),
            );
            state.tasks.push(task);

            task_id
        });
        crate::annotations::record_task_created(task_id, false);
        task_id
    }

    // Note: `spawn_thread`, `spawn_main_thread`, and `spawn_future` share some similar logic.
    // Changes to one of these functions likely need to be propagated to the other two as well.
    /// Spawn a new task for a future. This doesn't create a yield point; the caller should do that
    /// if it wants to give the new task a chance to run immediately.
    pub(crate) fn spawn_future<F>(
        future: F,
        stack_size: usize,
        name: Option<String>,
        caller: &'static Location<'static>,
    ) -> TaskId
    where
        F: Future<Output = ()> + 'static,
    {
        thread::switch();
        let task_id = Self::with(|state| {
            let schedule_len = CurrentSchedule::len();
            let parent_span_id = state.top_level_span.id();

            let task_id = TaskId(state.tasks.len());
            let tag = state.get_tag_or_default_for_current_task();

            Self::set_labels_for_new_task(state, task_id, name.clone());

            let clock = state.increment_clock_mut(); // Increment the parent's clock
            clock.extend(task_id); // and extend it with an entry for the new task

            let task = Task::from_future(
                future,
                stack_size,
                task_id,
                name,
                clock.clone(),
                parent_span_id,
                schedule_len,
                tag,
                Some(state.current().id()),
                state.current_mut().signature.new_child(caller),
            );

            state.tasks.push(task);

            task_id
        });
        crate::annotations::record_task_created(task_id, true);
        task_id
    }

    // Note: `spawn_thread`, `spawn_main_thread`, and `spawn_future` share some similar logic.
    // Changes to one of these functions likely need to be propagated to the other two as well.
    pub(crate) fn spawn_thread(
        f: Box<dyn FnOnce() + 'static>,
        stack_size: usize,
        name: Option<String>,
        mut initial_clock: Option<VectorClock>,
        caller: &'static Location<'static>,
    ) -> TaskId {
        thread::switch();
        let task_id = Self::with(|state| {
            let parent_span_id = state.top_level_span.id();
            let task_id = TaskId(state.tasks.len());
            let tag = state.get_tag_or_default_for_current_task();

            Self::set_labels_for_new_task(state, task_id, name.clone());

            let clock = if let Some(ref mut clock) = initial_clock {
                clock
            } else {
                // Inherit the clock of the parent thread (which spawned this task)
                state.increment_clock_mut()
            };
            clock.extend(task_id); // and extend it with an entry for the new thread
            let clock = clock.clone();

            let task = Task::from_closure(
                f,
                stack_size,
                task_id,
                name,
                clock,
                parent_span_id,
                CurrentSchedule::len(),
                tag,
                Some(state.current().id()),
                state.current_mut().signature.new_child(caller),
            );
            state.tasks.push(task);

            task_id
        });
        crate::annotations::record_task_created(task_id, false);
        task_id
    }

    /// Prepare this ExecutionState to be dropped. Call this before dropping so that the tasks have
    /// a chance to run their drop handlers while `EXECUTION_STATE` is still in scope.
    fn cleanup() {
        // A slightly delicate dance here: we need to drop the tasks from outside of `Self::with`,
        // because a task's Drop impl might want to call back into `ExecutionState` (to check
        // `should_stop()`). So we pull the tasks out of the `ExecutionState`, leaving it in an
        // invalid state, but no one should still be accessing the tasks anyway.
        let (mut tasks, final_state) = Self::with(|state| {
            state.in_cleanup = true;
            assert!(state.current_task == ScheduledTask::Stopped || state.current_task == ScheduledTask::Finished);
            (std::mem::take(&mut state.tasks), state.current_task)
        });

        for task in tasks.drain(..) {
            assert!(
                final_state == ScheduledTask::Stopped || task.finished() || task.detached,
                "execution finished but task is not"
            );
            Rc::try_unwrap(task.continuation)
                .map_err(|_| ())
                .expect("couldn't cleanup a future");
        }

        while Self::with(|state| state.storage.pop()).is_some() {}

        TASK_ID_TO_TAGS.with(|cell| cell.borrow_mut().clear());
        LABELS.with(|cell| cell.borrow_mut().clear());

        #[cfg(debug_assertions)]
        Self::with(|state| state.has_cleaned_up = true);

        Self::with(|state| state.in_cleanup = false);
    }

    /// Determine whether the execution has finished.
    pub(crate) fn is_finished(&self) -> bool {
        self.current_task == ScheduledTask::Stopped || self.current_task == ScheduledTask::Finished
    }

    /// Invoke the scheduler to decide which task to schedule next. Returns true if the chosen task
    /// is different from the currently running task, indicating that the current task should yield
    /// its execution.
    pub(crate) fn maybe_yield() -> bool {
        Self::with(|state| {
            if std::thread::panicking() && !state.in_cleanup {
                return true;
            }

            debug_assert!(
                matches!(state.current_task, ScheduledTask::Some(_) | ScheduledTask::Finished)
                    && state.next_task == ScheduledTask::None,
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
    pub(crate) fn failing_task() -> String {
        Self::try_with(|state| {
            if let Some(task) = state.try_current() {
                task.name().unwrap_or_else(|| format!("task-{:?}", task.id().0))
            } else {
                "<unknown>".into()
            }
        })
        .unwrap_or_else(|e| format!("Tried to get ExecutionState, but got the following error: {e:?}"))
    }

    /// Generate a random u64 from the current scheduler and return it.
    #[inline]
    pub(crate) fn next_u64() -> u64 {
        Self::with(|state| {
            CurrentSchedule::push_random();
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

    pub(crate) fn in_cleanup(&self) -> bool {
        self.in_cleanup
    }

    pub(crate) fn context_switches() -> usize {
        Self::with(|state| state.context_switches)
    }

    #[track_caller]
    pub(crate) fn new_resource_signature(resource_type: ResourceType) -> ResourceSignature {
        ExecutionState::with(|s| s.current_mut().signature.new_resource(resource_type))
    }

    pub(crate) fn get_storage<K: Into<StorageKey>, T: 'static>(&self, key: K) -> Option<&T> {
        self.storage
            .get(key.into())
            .map(|result| result.expect("global storage is never destructed"))
    }

    pub(crate) fn init_storage<K: Into<StorageKey>, T: 'static>(&mut self, key: K, value: T) {
        self.storage.init(key.into(), value);
    }

    pub(crate) fn get_clock(&self, id: TaskId) -> &VectorClock {
        &self.tasks.get(id.0).unwrap().clock
    }

    pub(crate) fn get_clock_mut(&mut self, id: TaskId) -> &mut VectorClock {
        &mut self.tasks.get_mut(id.0).unwrap().clock
    }

    /// Increment the current thread's clock entry and update its clock with the one provided.
    pub(crate) fn update_clock(&mut self, clock: &VectorClock) {
        let task = self.current_mut();
        task.clock.increment(task.id);
        task.clock.update(clock);
    }

    /// Increment the current thread's clock and return a shared reference to it
    pub(crate) fn increment_clock(&mut self) -> &VectorClock {
        let task = self.current_mut();
        task.clock.increment(task.id);
        &task.clock
    }

    /// Increment the current thread's clock and return a mutable reference to it
    pub(crate) fn increment_clock_mut(&mut self) -> &mut VectorClock {
        let task = self.current_mut();
        task.clock.increment(task.id);
        &mut task.clock
    }

    /// Returns `true` if the test has exceeded the step bound, and `false` otherwise.
    fn is_step_bound_exceeded(&self, max_steps: usize) -> bool {
        CurrentSchedule::len() - self.steps_reset_at >= max_steps
    }

    /// Run the scheduler to choose the next task to run. `has_yielded` should be false if the
    /// scheduler is being invoked from within a running task. If scheduling fails, returns an Err
    /// with a String describing the failure.
    fn schedule(&mut self) -> Result<(), StepError> {
        // Don't schedule twice. If `maybe_yield` ran the scheduler, we don't want to run it
        // again at the top of `step`.
        if self.next_task != ScheduledTask::None {
            return Ok(());
        }

        self.context_switches += 1;

        match self.config.max_steps {
            MaxSteps::FailAfter(max_steps) => {
                if self.is_step_bound_exceeded(max_steps) {
                    return Err(StepError::StepBoundExceeded);
                }
            }
            MaxSteps::ContinueAfter(max_steps) => {
                if self.is_step_bound_exceeded(max_steps) {
                    // TODO: We have to set `Stopped` and return `Ok` here, else assertions will fail. This should probably be cleaned up.
                    self.next_task = ScheduledTask::Stopped;
                    return Ok(());
                }
            }
            MaxSteps::None => {}
        }

        let mut unfinished_attached = false;
        let mut all_runnable_detached = true;
        let mut any_runnable = false;

        for task in &self.tasks {
            unfinished_attached |= !task.finished() && !task.detached;
            let is_runnable = task.runnable();
            any_runnable |= is_runnable;

            if is_runnable {
                all_runnable_detached &= task.detached;
                self.runnable_tasks.push(task as *const Task);
            } else if task.can_spuriously_wakeup() {
                // Some blocked tasks can be woken up spuriously, even though the condition the task is
                // blocked on hasn't happened yet. We'll add such tasks to the list of runnable tasks, but
                // they won't contribute to the check on `any_runnable`; if the only runnable tasks
                // are ones that are waiting for a potential spurious wakeup, it should still be treated as
                // a deadlock since there's no guarantee that spurious wakeups will ever occur.
                self.runnable_tasks.push(task as *const Task);
            }
        }

        // We should finish execution when either
        // (1) There are no runnable tasks, or
        // (2) All runnable tasks have been detached AND there are no unfinished attached tasks
        // If there are some unfinished attached tasks and all runnable tasks are detached, we must
        // run some detached task to give them a chance to unblock some unfinished attached task.
        if !any_runnable || (!unfinished_attached && all_runnable_detached) {
            self.next_task = ScheduledTask::Finished;
            return Ok(());
        }

        let is_yielding = std::mem::replace(&mut self.has_yielded, false);

        // Cast the slice of raw pointers to a slice of references in place to provide schedulers with a safe API
        //
        // SAFETY: This is safe because the tasks themselves are only being accessed through this shared reference by the
        // schedulers, and all references are always cleared from the runnable_tasks Vec at the end of this function.
        // The transmute itself is safe because *const and & have the same layout, and the pointer is created from a
        // reference earlier in this function.
        let task_refs = unsafe { std::mem::transmute::<&[*const Task], &[&Task]>(&self.runnable_tasks) };

        self.next_task = self
            .scheduler
            .borrow_mut()
            .next_task(task_refs, self.current_task.id(), is_yielding)
            .map(ScheduledTask::Some)
            .unwrap_or(ScheduledTask::Stopped);

        // Tracing this `in_scope` is purely a matter of taste. We do it because
        // 1) It is an action taken by the scheduler, and should thus be traced under the scheduler's span
        // 2) It creates a visual separation of scheduling decisions and `Task`-induced tracing.
        // Note that there is a case to be made for not `in_scope`-ing it, as that makes seeing the context
        // of the context switch clearer.
        //
        // Note also that changing this trace! statement requires changing the test `basic::labels::test_tracing_with_label_fn`
        // which relies on this trace reporting the `runnable` tasks.
        self.top_level_span.in_scope(|| {
            trace!(
                i=CurrentSchedule::len(),
                next_task=?self.next_task,
                runnable=?task_refs.iter().map(|task| task.id()).collect::<SmallVec<[_; DEFAULT_INLINE_TASKS]>>(),
                "scheduling decision"
            );
        });

        // If the task chosen by the scheduler is blocked, then it should be one that can be
        // spuriously woken up, and we need to unblock it here so that it can execute.
        if let Some(tid) = self.next_task.id() {
            let task = self.get_mut(tid);
            assert!(task.runnable() || task.blocked());
            if task.blocked() {
                assert!(task.can_spuriously_wakeup());
                task.unblock();
            }
        }

        // Retains the capacity of `runnable_tasks` for future calls of `schedule`
        self.runnable_tasks.clear();

        Ok(())
    }

    /// Set the next task as the current task
    fn advance_to_next_task(&mut self) {
        debug_assert_ne!(self.next_task, ScheduledTask::None);
        self.current_task = self.next_task.take();

        if let ScheduledTask::Some(tid) = self.current_task {
            CurrentSchedule::push_task(tid);
        }
    }

    // Sets the `tag` field of the current task.
    // Returns the `tag` which was there previously.
    #[allow(deprecated)]
    pub(crate) fn set_tag_for_current_task(tag: Arc<dyn Tag>) -> Option<Arc<dyn Tag>> {
        ExecutionState::with(|s| s.current_mut().set_tag(tag))
    }

    #[allow(deprecated)]
    fn get_tag_or_default_for_current_task(&self) -> Option<Arc<dyn Tag>> {
        self.try_current().and_then(|current| current.get_tag())
    }

    #[allow(deprecated)]
    pub(crate) fn get_tag_for_current_task() -> Option<Arc<dyn Tag>> {
        ExecutionState::with(|s| s.get_tag_or_default_for_current_task())
    }

    #[allow(deprecated)]
    pub(crate) fn set_tag_for_task(task: TaskId, tag: Arc<dyn Tag>) -> Option<Arc<dyn Tag>> {
        ExecutionState::with(|s| s.get_mut(task).set_tag(tag))
    }
}

#[cfg(debug_assertions)]
impl Drop for ExecutionState {
    fn drop(&mut self) {
        assert!(self.has_cleaned_up || std::thread::panicking());
    }
}
