use crate::config::DEFAULT_MAX_STEPS;
use crate::runtime::failure::{init_panic_hook, persist_failure};
use crate::runtime::storage::{StorageKey, StorageMap};
use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::labels::Labels;
use crate::runtime::task::{ChildLabelFn, ParkedDefault, Task, TaskId, TaskName, TaskSignature, DEFAULT_INLINE_TASKS};
use crate::runtime::thread;
use crate::runtime::thread::continuation::PooledContinuation;
use crate::scheduler::{Schedule, Scheduler};
use crate::sync_types::{ResourceSignature, ResourceType};
use crate::thread_support::thread_fn;
use crate::{backtrace_enabled, Config, MaxSteps, UNGRACEFUL_SHUTDOWN_CONFIG};
use scoped_tls::scoped_thread_local;
use smallvec::SmallVec;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::future::Future;
use std::panic::{self, Location};
use std::rc::Rc;
use std::sync::Arc;
use tracing::subscriber::NoSubscriber;
use tracing::{trace, Span};

#[allow(deprecated)]
use super::task::Tag;

// We use this scoped TLS to smuggle the ExecutionState, which is not 'static, across tasks that
// need access to it (to spawn new tasks, interrogate task status, etc).
scoped_thread_local! {
    static EXECUTION_STATE: RefCell<ExecutionState>
}

thread_local! {
    // Whether the panic hook stays silent, because execution teardown ignores the panics it catches
    // now (see `ExecutionState::tear_down`). Outside `EXECUTION_STATE`, so that the hook can read it
    // while the state is borrowed.
    static TEARDOWN_IGNORES_PANICS: Cell<bool> = const { Cell::new(false) };
    // How many panics there have been in the current step of execution teardown, for the panic
    // hook (see `ExecutionState::teardown_ignores_panic`).
    static TEARDOWN_STEP_PANICS: Cell<usize> = const { Cell::new(0) };
}

thread_local! {
    // Whether this thread is emitting the `ExecutionState::try_with` event. A `tracing` subscriber
    // may access the state while it handles an event (to call `current::clock()`, say), and that
    // access must not emit the event again: `tracing` only refuses re-entrant calls for scoped
    // subscribers, and with a global one this would recurse until the stack overflows.
    static TRACING_TRY_WITH: Cell<bool> = const { Cell::new(false) };
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
    pub fn len() -> usize {
        CURRENT_SCHEDULE.with(|cs| (*cs.current_schedule.borrow()).len())
    }

    /// Returns a clone of the inner schedule
    pub fn get_schedule() -> Schedule {
        CURRENT_SCHEDULE.with(|cs| (*cs.current_schedule.borrow()).clone())
    }
}

thread_local! {
    #[allow(clippy::complexity)]
    #[allow(deprecated)]
    pub static TASK_ID_TO_TAGS: RefCell<HashMap<TaskId, Arc<dyn Tag>>> = RefCell::new(HashMap::new());
}

thread_local! {
    pub static LABELS: RefCell<HashMap<TaskId, Labels>> = RefCell::new(HashMap::new());
}

/// An `Execution` encapsulates a single run of a function under test against a chosen scheduler.
/// Its only useful method is `Execution::run`, which executes the function to completion.
///
/// The key thing that an `Execution` manages is the `ExecutionState`, which contains all the
/// mutable state a test's tasks might need access to during execution (to block/unblock tasks,
/// spawn new tasks, etc). The `Execution` makes this state available through the `EXECUTION_STATE`
/// static variable, but clients get access to it by calling `ExecutionState::with`.
pub struct Execution {
    scheduler: Rc<RefCell<dyn Scheduler>>,
    initial_schedule: Schedule,
}

impl std::fmt::Debug for Execution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Execution").finish_non_exhaustive()
    }
}

impl Execution {
    /// Construct a new execution that will use the given scheduler. The execution should then be
    /// invoked via its `run` method, which takes as input the closure for task 0.
    pub fn new(scheduler: Rc<RefCell<dyn Scheduler>>, initial_schedule: Schedule) -> Self {
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
    // We exceeded the step bound of `MaxSteps::FailAfter`, which this is.
    StepBoundExceeded(usize),
    // Task panic and `config.immediately_return_on_panic` is set to `true`.
    TaskPanicEarlyReturn,
}

/// How a failed execution fails the test, once it has been torn down.
enum Failure {
    /// Resume unwinding with the panic payload.
    Unwind(Box<dyn Any + Send>),
    /// Panic with the message.
    Panic(String),
    /// Panic with the message, as a `&'static str` payload.
    StaticPanic(&'static str),
}

/// How tearing down an execution that had not failed fails the test (see `ExecutionState::tear_down`).
enum TeardownFailure {
    /// A destructor panicked: resume unwinding with its payload.
    Unwind(Box<dyn Any + Send>),
    /// A task that was unwinding a panic when the execution stopped has finished unwinding it:
    /// resume unwinding with its payload.
    LatePanic(Box<dyn Any + Send>),
    /// Panic with the message: destructors blocked, and nothing could wake them, say.
    Panic(String),
}

impl Execution {
    /// Run a function to be tested, taking control of scheduling it and any tasks it might spawn.
    /// This function runs until `f` and all tasks spawned by `f` have terminated, or until the
    /// scheduler returns `None`, indicating the execution should not be explored any further.
    pub fn run<F>(mut self, config: &Config, f: F, caller: &'static Location<'static>)
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

            // Run the test to completion. A panic out of the executor (from a scheduler, say) fails
            // the execution like a task's panic does, so that the execution is still torn down while
            // Shuttle is there to call into.
            let immediately_return_on_panic = UNGRACEFUL_SHUTDOWN_CONFIG.get().immediately_return_on_panic;
            let failure = match panic::catch_unwind(panic::AssertUnwindSafe(|| {
                self.run_to_completion(immediately_return_on_panic)
            })) {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(Self::failure(e, config)),
                // The panic hook has reported it.
                Err(payload) => Some(Failure::Unwind(payload)),
            };

            // Tear down the execution before it goes out of `EXECUTION_STATE` scope. A failed
            // execution is torn down before its failure is raised, so that nothing that the failure
            // leaves to unwind can call into Shuttle once `EXECUTION_STATE` is gone.
            let kind = if failure.is_some() {
                TeardownKind::Failed
            } else if ExecutionState::execution_stopped() {
                TeardownKind::Stopped
            } else {
                TeardownKind::Finished
            };
            let teardown_failure = ExecutionState::tear_down(kind);
            match failure {
                Some(Failure::Unwind(payload)) => panic::resume_unwind(payload),
                Some(Failure::Panic(message)) => panic::panic_any(message),
                Some(Failure::StaticPanic(message)) => panic::panic_any(message),
                None => match teardown_failure {
                    Some(TeardownFailure::Unwind(payload)) => {
                        persist_failure(config);
                        eprintln!("test panicked while dropping the tasks that were unfinished at the end of the execution");
                        panic::resume_unwind(payload);
                    }
                    Some(TeardownFailure::LatePanic(payload)) => {
                        persist_failure(config);
                        eprintln!("test panicked in a task that was still unwinding the panic when the execution stopped");
                        panic::resume_unwind(payload);
                    }
                    Some(TeardownFailure::Panic(message)) => panic::panic_any(message),
                    None => {}
                },
            }
        });
    }

    /// Report the failure of the execution, and return how to fail the test with it once the
    /// execution has been torn down.
    fn failure(e: StepError, config: &Config) -> Failure {
        persist_failure(config);
        // Teardown changes the current task, so remember which task failed for the panic hook.
        ExecutionState::with(|state| state.failed_task = state.try_current().map(Task::display_name));

        match e {
            StepError::TaskFailure(payload) => {
                eprintln!("test panicked in task '{}'", ExecutionState::failing_task());

                Failure::Unwind(payload)
            }
            StepError::Deadlock => {
                let blocked_tasks = ExecutionState::with(|state| {
                    state
                        .tasks
                        .iter()
                        .filter(|t| !t.finished())
                        .map(|t| t.format_for_deadlock())
                        .collect::<Vec<_>>()
                });

                // Collecting backtraces is expensive, so we only want to do it if the user opts in to collecting them.
                if !backtrace_enabled() {
                    eprintln!("Test deadlocked, and {} is not set. If either of those are set then the backtrace of each task will be collected and printed as part of the panic message.", crate::CAPTURE_BACKTRACE)
                }

                Failure::Panic(format!("deadlock! blocked tasks: [{}]", blocked_tasks.join(", ")))
            }
            StepError::SchedulingError => {
                Failure::StaticPanic("no task was scheduled\nThis indicates an issue with the scheduler.")
            }
            StepError::StepBoundExceeded(max_steps) => Failure::Panic(format!(
                "exceeded max_steps bound {max_steps}. this might be caused by an unfair schedule (e.g., a spin loop)?"
            )),
            StepError::TaskPanicEarlyReturn => Failure::Unwind(Box::new("Task panicked, and early return is enabled.")),
        }
    }

    fn enter_task_span() {
        // Enter the Task's span
        // (Note that if any issues arise with spans and tracing, then
        // 1) calling `exit` until `None` before entering the `Task`s `Span`,
        // 2) storing the entirety of the `span_stack` when creating the `Task`, and
        // 3) storing `top_level_span` as a stack
        // should be tried.)
        let parked = ExecutionState::with(|state| {
            // Go through `Span::with_subscriber` rather than calling `Span::current()` inside
            // `tracing::dispatcher::get_default`; see `exit_task_span` for why.
            tracing::Span::current().with_subscriber(|(id, subscriber)| subscriber.exit(id));

            // The `span_stack` stores `Span`s such that the top of the stack is the outermost `Span`,
            // meaning that parents (left-most when printed) are entered first.
            while let Some(span) = state.current_mut().span_stack.pop() {
                span.with_subscriber(|(id, subscriber)| subscriber.enter(id));
            }

            if state.config.record_steps_in_span {
                state.current().step_span.record("i", CurrentSchedule::len());
            }

            state.current_mut().parked_default.take()
        });

        // Reinstate the default dispatcher the task had when it last switched out. Outside
        // `ExecutionState::with`, as that drops the dispatcher it displaces.
        if let Some(parked) = parked {
            parked.reinstate();
        }
    }

    fn exit_task_span(yielded: bool) {
        // Leave the Task's span and store the exited `Span` stack in order to restore it the next time the Task is run
        ExecutionState::with(|state| {
            // Before the task's spans are exited, so they are looked up in the dispatcher they were entered in.
            if yielded {
                Execution::park_task_default(state);
            }

            debug_assert!(state.current().span_stack.is_empty());
            // Note that `Span::current()` must not be called from inside a
            // `tracing::dispatcher::get_default` callback: `get_default` marks the thread's
            // dispatcher state as in use for the duration of the callback, and while any thread in
            // the process holds a scoped default subscriber, a nested `Span::current()` then
            // returns `Span::none()`. This loop would then exit nothing while we still enter
            // `top_level_span` below, leaking one entry per scheduling step until a later
            // `Span::current()` resolved to a closed span and panicked. So each exit goes through
            // `Span::with_subscriber`, which hands us the span's own dispatcher directly.
            // `Span::current()` returns a disabled span once no span is entered, and a disabled span
            // is exactly one `with_subscriber` does nothing for, so every iteration exits a span.
            while let Some(current) = Some(tracing::Span::current()).filter(|span| !span.is_disabled()) {
                current.with_subscriber(|(id, subscriber)| subscriber.exit(id));
                state.current_mut().span_stack.push(current);
            }

            state
                .top_level_span
                .with_subscriber(|(id, subscriber)| subscriber.enter(id));
        });
    }

    /// Saves the running task's default `tracing` dispatcher and reinstates the execution's.
    ///
    /// `tracing`'s default dispatcher is per OS thread, and every task runs on this one. Without
    /// this, a task that yields inside `tracing::subscriber::with_default` would leave its dispatcher
    /// installed for the scheduler and every other task, and `exit_task_span` would look for the
    /// task's spans in that dispatcher instead of the one they were entered in.
    fn park_task_default(state: &mut ExecutionState) {
        // Installing a guard when there is nothing to park would put `tracing` on its slower scoped
        // path for the whole process.
        if !task_may_have_own_default(state) {
            return;
        }
        // The guard remembers the task's current default, and `enter_task_span` drops it to
        // reinstate exactly that.
        let guard = tracing::dispatcher::set_default(&state.top_level_dispatch);
        state.current_mut().parked_default = Some(ParkedDefault::new(guard));
    }

    /// Run the execution to completion.
    #[inline]
    fn run_to_completion(&mut self, immediately_return_on_panic: bool) -> Result<(), StepError> {
        loop {
            let next_step: Option<Rc<RefCell<PooledContinuation>>> = ExecutionState::with(|state| {
                state.schedule()?;
                state.advance_to_next_task();

                match state.current_task {
                    ScheduledTask::Some(tid) => {
                        let task = state.get(tid);
                        Ok(Some(
                            task.continuation
                                .clone()
                                .expect("only execution teardown takes a task's continuation"),
                        ))
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

                    Execution::exit_task_span(matches!(result, Ok(false)));

                    result
                }
                None => return Ok(()),
            };

            match ret {
                // Task finished
                Ok(true) => {
                    crate::annotations::record_task_terminated();
                    ExecutionState::with(|state| {
                        state.finish_current_task();
                        // The task may have caught the panic that was unwinding last, and finished
                        // without another scheduling point. Here, between steps,
                        // `std::thread::panicking()` says exactly whether a task that switched out is
                        // unwinding a panic still (see `record_unwinding_task`).
                        if state.switched_out_unwinding && !std::thread::panicking() {
                            state.forget_unwinding_task();
                        }
                    });
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
pub struct ExecutionState {
    pub config: Config,
    // invariant: tasks are never removed from this list, until execution teardown drops them all
    tasks: SmallVec<[Task; DEFAULT_INLINE_TASKS]>,
    // invariant: if this transitions to Stopped or Finished, it can never change again, except that
    // execution teardown makes a failed execution Stopped, and makes each task that it tears down the
    // current task while it does (see `tear_down`)
    current_task: ScheduledTask,
    // the task the scheduler has chosen to run next
    next_task: ScheduledTask,
    // whether the current task has asked to yield
    has_yielded: bool,
    // the number of scheduling decisions made so far
    context_switches: usize,
    // the schedule length last time `reset_stop_bound()` was called
    pub steps_reset_at: usize,

    // static values for the current execution
    storage: StorageMap,

    scheduler: Rc<RefCell<dyn Scheduler>>,

    // the state of execution teardown, while the execution is being torn down (see `tear_down`)
    teardown: Option<Teardown>,

    // the name of the task that failed the execution, if a task did, for the panic hook to report
    // once the execution has been torn down (see `failing_task`)
    failed_task: Option<String>,

    // the task that switched out while unwinding a panic, if it may be unwinding it still, with
    // whether it is detached otherwise: it is attached until it has finished unwinding (see
    // `record_unwinding_task`)
    unwinding_task: Option<(TaskId, bool)>,

    // whether a task has switched out while unwinding a panic since `std::thread::panicking()` was
    // last false: if not, no task but the current one can be unwinding a panic (see
    // `record_unwinding_task`)
    switched_out_unwinding: bool,

    #[cfg(debug_assertions)]
    has_cleaned_up: bool,

    // The `Span` which the `ExecutionState` was created under. Will be the parent of all `Task` `Span`s
    pub top_level_span: Span,

    // The default `tracing` dispatcher the `ExecutionState` was created under. Tasks start out with it,
    // and it is reinstated whenever a task with a different default switches out.
    top_level_dispatch: tracing::Dispatch,

    // If `top_level_dispatch` is the global default, where `get_default` hands it out from (see
    // `task_may_have_own_default`).
    top_level_dispatch_global_ptr: Option<*const tracing::Dispatch>,

    // Persistent Vec used as a bump allocator for references to runnable tasks to avoid slow allocation
    // on each scheduling decision. Should not be used outside of the `schedule` function
    runnable_tasks: Vec<*const Task>,

    // Ids of all tasks that have not yet finished, kept sorted in ascending order.
    //
    // `tasks` never shrinks, so it accumulates every task ever created by the execution. Scanning it
    // on every scheduling decision therefore costs O(tasks ever created), even though only the
    // unfinished ones can ever be scheduled. This set lets `schedule` iterate just the live tasks.
    //
    // invariant: contains exactly the ids of the tasks in `tasks` that are not `Finished`, in
    // ascending order. Maintained by pushing on task creation (ids are handed out sequentially, so
    // pushing keeps this sorted) and removing in `finish_current_task`. This is sound because
    // `Finished` is a terminal state: it is only ever set by `Task::finish`, and `block`, `sleep`,
    // and `unblock` all assert that they are never applied to a finished task. (Execution teardown
    // schedules nothing, and doesn't maintain this: see `tear_down`. It also lets a finished task
    // stand in for a while: see `Task::stand_in`.)
    live_tasks: Vec<TaskId>,
}

impl std::fmt::Debug for ExecutionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionState").finish_non_exhaustive()
    }
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

/// How an execution ended, which decides how it is torn down (see `ExecutionState::tear_down`).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum TeardownKind {
    /// Every attached task finished.
    Finished,
    /// The scheduler stopped the execution before that.
    Stopped,
    /// The execution failed. Its failure is raised once it has been torn down.
    Failed,
}

impl TeardownKind {
    /// Whether the execution is abandoned, rather than torn down as if its tasks were cancelled.
    fn abandons(self) -> bool {
        self != Self::Finished
    }

    /// What `ExecutionState::current_task` is between the steps of teardown.
    fn final_state(self) -> ScheduledTask {
        match self {
            Self::Finished => ScheduledTask::Finished,
            Self::Stopped | Self::Failed => ScheduledTask::Stopped,
        }
    }
}

/// What execution teardown is doing (see `ExecutionState::tear_down`). In each step, the task named
/// is the current task.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum TeardownStep {
    /// Nothing, between two steps.
    Idle,
    /// The task runs on its own stack, cancelled: it drops its future, or the function it never ran.
    Cancel(TaskId),
    /// The task's stack is unwound.
    Unwind(TaskId),
    /// The task runs on its own stack to finish unwinding a panic (see
    /// `ExecutionState::finish_unwinding`).
    FinishUnwinding(TaskId),
    /// What the task left behind, its task-local values say, is dropped on the executor's stack.
    Leftovers(TaskId),
    /// A static is dropped on the executor's stack, with the main thread standing in.
    Static,
}

/// The state of execution teardown (see `ExecutionState::tear_down`).
#[derive(Debug)]
struct Teardown {
    kind: TeardownKind,
    step: TeardownStep,
    /// The scheduling points that destructors have reached, which the step bound limits.
    steps: usize,
    /// The step bound, once a destructor has exceeded it. Then the rest of the execution is
    /// abandoned (see `next_teardown_job`).
    exceeded_step_bound: Option<usize>,
    /// Whether the current task runs on its own stack now, so that it can switch out (see
    /// `maybe_yield_in_teardown`).
    on_own_stack: bool,
    /// The state of the generator that destructors draw random numbers from, instead of the
    /// scheduler, so that teardown doesn't extend the schedule but is the same when it is replayed.
    rng: u64,
}

impl Teardown {
    fn new(kind: TeardownKind, schedule_len: usize) -> Self {
        Self {
            kind,
            step: TeardownStep::Idle,
            steps: 0,
            exceeded_step_bound: None,
            on_own_stack: false,
            rng: schedule_len as u64,
        }
    }

    /// The next number from the generator (SplitMix64).
    fn next_u64(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Where execution teardown has got to (see `ExecutionState::next_teardown_job`).
#[derive(Debug, Default)]
struct TeardownPlan {
    /// The task that switched out while it unwound a panic, and is unwinding it still (see
    /// `ExecutionState::finish_unwinding`).
    unwinding_task: Option<TaskId>,
    /// The index in `ExecutionState::tasks` of the next task that teardown hasn't looked at.
    next_task: usize,
    /// The tasks that teardown cancelled and that switched out, in the order they did, each with
    /// whether it blocked (rather than yielded).
    suspended: Vec<(TaskId, bool)>,
    /// The unfinished tasks in the middle of user code, whose stacks teardown unwinds or leaks.
    stacks: VecDeque<TaskId>,
    /// How many times in a row a task that yielded has been resumed without any task finishing or
    /// blocking since, while stacks are waiting to be unwound.
    idle_yields: usize,
}

/// How many turns each task that yields gets before a stack is unwound, if no task finishes or
/// blocks meanwhile (see `ExecutionState::next_teardown_job`), unless that would take more than a
/// quarter of the steps that the step bound leaves. The tasks that yield may wait for a task whose
/// stack teardown is going to unwind, or that task's destructors may wait for a lock that they
/// hold; unwinding a stack can't wait.
const TEARDOWN_YIELD_ROUNDS: usize = 1000;

/// How execution teardown resumes a task on the task's own stack (see
/// `ExecutionState::run_on_own_stack`).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Resumption {
    /// Cancel the task (see `Continuation::cancel`).
    Cancel,
    /// Resume a cancelled task that switched out.
    Resume,
}

/// What execution teardown does next (see `ExecutionState::next_teardown_job`).
enum TeardownJob {
    /// Let the task finish unwinding a panic.
    FinishUnwinding(TaskId),
    /// Resume the task on its own stack.
    OnOwnStack(TaskId, Resumption),
    /// Unwind the task's stack.
    Unwind(TaskId),
    /// Leak the task's stack.
    Leak(TaskId),
    /// Leak the function of a task that never ran.
    LeakFunction(TaskId),
    /// Drop task-local values that a finished task left.
    TaskLocals(TaskId),
    /// Drop a static.
    Static(Box<dyn Any>),
    /// Every cancelled task that hasn't finished blocked, and nothing can wake them, or teardown is
    /// abandoning them.
    Stuck,
    /// A destructor exceeded the step bound, which this is: abandon the rest of the execution.
    Abandon(usize),
    Done,
}

/// Why execution teardown stops a destructor (see `ExecutionState::maybe_yield_in_teardown`).
enum TeardownStall {
    /// It blocked where it cannot wait.
    Blocked,
    /// It exceeded the step bound, which this is.
    ExceededStepBound(usize),
}

/// How execution teardown is going to fail the test, if it does (see `ExecutionState::tear_down`).
struct TeardownReport {
    failure: Option<TeardownFailure>,
    ignored_panics: usize,
}

impl TeardownReport {
    fn new() -> Self {
        Self {
            failure: None,
            ignored_panics: 0,
        }
    }

    /// Run `f`, which drops something, and deal with a panic from it.
    fn catch(&mut self, f: impl FnOnce()) {
        TEARDOWN_STEP_PANICS.set(0);
        if let Err(payload) = panic::catch_unwind(panic::AssertUnwindSafe(f)) {
            self.panicked(payload);
        }
    }

    /// A destructor panicked. The first panic while tearing down a finished execution fails the
    /// test, and teardown ignores any other.
    fn panicked(&mut self, payload: Box<dyn Any + Send>) {
        let abandoned = ExecutionState::with(|state| state.teardown().kind.abandons());
        if !abandoned && self.failure.is_none() {
            self.failure = Some(TeardownFailure::Unwind(payload));
            // The panic hook has reported this one, but stays silent for those ignored from now on.
            TEARDOWN_IGNORES_PANICS.set(true);
        } else {
            self.ignored_panics += 1;
            // Dropping the payload can panic too.
            if let Err(payload) = panic::catch_unwind(panic::AssertUnwindSafe(move || drop(payload))) {
                std::mem::forget(payload);
            }
        }
    }

    /// A task that was unwinding a panic when the execution stopped has finished unwinding it. That
    /// panic fails the test, unless something else already does.
    fn late_panic(&mut self, payload: Box<dyn Any + Send>) {
        if self.failure.is_none() {
            self.failure = Some(TeardownFailure::LatePanic(payload));
        } else {
            self.panicked(payload);
        }
    }

    /// Fail the test with `message`, unless something else already does.
    fn fail(&mut self, message: String) {
        if self.failure.is_none() {
            self.failure = Some(TeardownFailure::Panic(message));
        }
    }

    /// The destructors that teardown of a finished execution has left all blocked, and nothing can
    /// wake them.
    fn deadlocked(&mut self, blocked_tasks: Vec<String>) {
        self.fail(format!(
            "deadlock while dropping the tasks that were unfinished at the end of the execution! blocked tasks: [{}]",
            blocked_tasks.join(", ")
        ));
    }

    fn finish(self) -> Option<TeardownFailure> {
        if self.failure.is_some() && self.ignored_panics > 0 {
            eprintln!(
                "{} more panics while dropping the tasks that were unfinished at the end of the execution were ignored",
                self.ignored_panics
            );
        }
        self.failure
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
        let (top_level_dispatch, top_level_dispatch_global_ptr) = tracing::dispatcher::get_default(|dispatch| {
            // While no thread has a scoped default, `get_default` hands out the global default, and
            // nested calls get it too rather than `Dispatch::none()`.
            let is_global = tracing::dispatcher::get_default(|nested| !nested.is::<NoSubscriber>());
            (dispatch.clone(), is_global.then_some(dispatch as *const _))
        });
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
            teardown: None,
            failed_task: None,
            unwinding_task: None,
            switched_out_unwinding: false,
            #[cfg(debug_assertions)]
            has_cleaned_up: false,
            top_level_span: tracing::Span::current(),
            top_level_dispatch,
            top_level_dispatch_global_ptr,
            runnable_tasks: Vec::with_capacity(DEFAULT_INLINE_TASKS),
            live_tasks: Vec::with_capacity(DEFAULT_INLINE_TASKS),
        }
    }

    /// Invoke a closure with access to the current execution state. Library code uses this to gain
    /// access to the state of the execution to influence scheduling (e.g. to register a task as
    /// blocked).
    #[inline]
    #[track_caller]
    pub fn with<F, T>(f: F) -> T
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
    pub fn try_with<F, T>(f: F) -> Result<T, ExecutionStateBorrowError>
    where
        F: FnOnce(&mut ExecutionState) -> T,
    {
        // Check that the event is enabled first, so that accesses don't pay for the flag when it isn't.
        if tracing::enabled!(tracing::Level::TRACE) && !TRACING_TRY_WITH.with(|emitting| emitting.replace(true)) {
            struct Reset;
            impl Drop for Reset {
                fn drop(&mut self) {
                    TRACING_TRY_WITH.with(|emitting| emitting.set(false));
                }
            }
            let _reset = Reset;
            trace!(
                "ExecutionState::try_with called from {:?}",
                std::panic::Location::caller()
            );
        }
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
    pub fn me() -> TaskId {
        Self::with(|s| s.current().id())
    }

    /// If there is only one attached, unfinished task and there is at least one detached, unfinished task
    /// then exiting the attached task will cause the whole execution to exit. As a result, the unfinished
    /// detached tasks are truncated -- their remaining events will not be executed because the program itself
    /// has exited. This is relevant because it means that *exiting* a task can be a visible operation
    /// in that it affects which events are executed.
    pub fn exit_current_truncates_execution(&self) -> bool {
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
    pub fn spawn_main_thread(
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
            state.add_task(task);

            task_id
        });
        crate::annotations::record_task_created(task_id, false);
        task_id
    }

    // Note: `spawn_thread`, `spawn_main_thread`, and `spawn_future` share some similar logic.
    // Changes to one of these functions likely need to be propagated to the other two as well.
    /// Spawn a new task for a future. This doesn't create a yield point; the caller should do that
    /// if it wants to give the new task a chance to run immediately.
    pub fn spawn_future<F>(
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

            state.add_task(task);

            task_id
        });
        crate::annotations::record_task_created(task_id, true);
        task_id
    }

    // Note: `spawn_thread`, `spawn_main_thread`, and `spawn_future` share some similar logic.
    // Changes to one of these functions likely need to be propagated to the other two as well.
    pub fn spawn_thread(
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
            state.add_task(task);

            task_id
        });
        crate::annotations::record_task_created(task_id, false);
        task_id
    }

    /// Tear down the execution, which ended as `kind` says, before it goes out of `EXECUTION_STATE`
    /// scope: drop what its unfinished tasks leave behind, the tasks' task-local values, and the
    /// execution's statics, while their destructors can still use Shuttle. Returns how that fails
    /// the test, if it does.
    ///
    /// A finished execution's unfinished tasks (detached future tasks, and tasks that destructors
    /// spawn during teardown) are torn down as if the execution cancelled them as it ended. While a
    /// task is torn down, it is the current task: its destructors can use Shuttle as the running
    /// task can, and what they do is attributed to it.
    ///
    /// * A task that never ran drops its function, and a future task that is parked between polls,
    ///   the usual state of an unfinished future task, drops its future, the way an async runtime
    ///   drops the futures of its tasks when it shuts down (see `Continuation::cancel`). They do
    ///   this on their own stacks, and then drop their task-local values. So a destructor that
    ///   blocks, on a lock that another unfinished task holds say, waits for teardown to drop that
    ///   task. A panic is an ordinary panic, which fails the test once teardown is done.
    /// * A future task that stopped in the middle of `poll` has user code on its stack, which only
    ///   unwinding the stack can drop. Teardown unwinds these stacks one at a time, once no other
    ///   task can make progress, so that the locks that the other tasks held are free. Their
    ///   destructors can use Shuttle too, but cannot wait: one that blocks, or panics, while a stack
    ///   is unwound aborts the process, as any panic during unwinding does. A task that catches the
    ///   unwind is unwound again at its next scheduling point.
    ///
    /// Nothing is scheduled. Teardown resumes the tasks it cancelled itself, in a fixed order that
    /// doesn't extend the schedule, so it is the same when the schedule is replayed (see
    /// `next_teardown_job`). A destructor that blocks waits until another destructor wakes it, and
    /// one that yields lets the others run; but if every destructor left blocks, nothing can wake
    /// them, and that fails the test like a deadlock. The step bound applies to destructors too (see
    /// `maybe_yield_in_teardown`). The execution's statics go last, as if the main thread dropped
    /// them, once nothing else is left that could still use them.
    ///
    /// A stopped or failed execution is abandoned instead. Shuttle's own destructors skip their
    /// bookkeeping (see `should_stop`), and panics are ignored, so that a failure is what gets
    /// reported. The stacks of its unfinished tasks are leaked. A failed execution's functions of
    /// tasks that never ran, and futures of parked future tasks, are leaked too, unless
    /// `UngracefulShutdownConfig::continuation_function_behavior` says to drop them, in which case
    /// they are cancelled as above. A stopped execution's functions of tasks that never ran are
    /// dropped that way. Every function that is dropped goes before any stack is freed, since a
    /// scoped thread's function borrows from its parent's stack. Task-local values and statics are
    /// dropped as in a finished execution.
    ///
    /// A task that was unwinding a panic when a stopped execution stopped finishes unwinding it first,
    /// and the panic fails the test (see `finish_unwinding`). A failed execution's is leaked like the
    /// rest, as running more of a failed execution risks a panic that aborts the process. If teardown
    /// can't tell which task it is (see `record_unwinding_task`), that fails the test too, and the
    /// rest of the execution is abandoned: unwinding that task's stack again would abort the process.
    fn tear_down(kind: TeardownKind) -> Option<TeardownFailure> {
        let mut plan = TeardownPlan::default();
        let mut report = TeardownReport::new();
        // Here, on the executor's stack, `std::thread::panicking()` says whether a task that switched
        // out is unwinding a panic still (see `record_unwinding_task`).
        let unwinding = std::thread::panicking();
        let lost_panic = Self::with(|state| {
            state.current_task = kind.final_state();
            state.next_task = ScheduledTask::None;
            state.teardown = Some(Teardown::new(kind, CurrentSchedule::len()));
            if kind != TeardownKind::Failed && unwinding {
                let unwinding_task = state.unwinding_task.take().map(|(id, _)| id);
                plan.unwinding_task = unwinding_task.filter(|&id| state.get(id).suspended());
            }
            let lost_panic = unwinding && plan.unwinding_task.is_none() && kind != TeardownKind::Failed;
            if lost_panic {
                // So that no stack is unwound.
                state.abandon_teardown();
            }
            lost_panic
        });
        if lost_panic {
            report.fail(
                "a task was still unwinding a panic when the execution ended, but it panicked while another task was \
                 unwinding a panic, so Shuttle can't tell which task it is, to let it finish unwinding"
                    .into(),
            );
        }
        // The panic hook goes back to reporting panics when teardown is done, however it ends.
        struct ReportPanicsAgain;
        impl Drop for ReportPanicsAgain {
            fn drop(&mut self) {
                TEARDOWN_IGNORES_PANICS.set(false);
            }
        }
        let report_panics_again = ReportPanicsAgain;
        TEARDOWN_IGNORES_PANICS.set(Self::with(|state| state.teardown().kind.abandons()));

        loop {
            match Self::with(|state| state.next_teardown_job(&mut plan)) {
                TeardownJob::FinishUnwinding(id) => Self::finish_unwinding(id, &mut report),
                TeardownJob::OnOwnStack(id, resumption) => {
                    Self::run_on_own_stack(id, resumption, &mut plan, &mut report)
                }
                TeardownJob::Unwind(id) => Self::unwind_task(id, &mut report),
                TeardownJob::Leak(id) => Self::leak_task(id, &mut report),
                TeardownJob::LeakFunction(id) => {
                    Self::with(|state| {
                        let task = state.get(id);
                        let continuation = task
                            .continuation
                            .as_ref()
                            .expect("a task that never ran has a continuation");
                        continuation.borrow_mut().leak_function();
                    });
                    Self::finish_torn_down_task(id, &mut report);
                }
                TeardownJob::TaskLocals(id) => Self::drop_leftover_task_locals(id, &mut report),
                TeardownJob::Static(value) => Self::drop_static(value, &mut report),
                TeardownJob::Stuck => {
                    let (deadlocked, blocked_tasks) = Self::with(|state| {
                        let teardown = state.teardown();
                        let deadlocked = !teardown.kind.abandons() && teardown.exceeded_step_bound.is_none();
                        let blocked = plan
                            .suspended
                            .iter()
                            .map(|&(id, _)| state.get(id).format_for_deadlock());
                        let blocked = blocked.collect::<Vec<_>>();
                        state.abandon_teardown();
                        (deadlocked, blocked)
                    });
                    TEARDOWN_IGNORES_PANICS.set(true);
                    if deadlocked {
                        report.deadlocked(blocked_tasks);
                    }
                    for (id, _) in std::mem::take(&mut plan.suspended) {
                        Self::leak_task(id, &mut report);
                    }
                }
                TeardownJob::Abandon(max_steps) => {
                    // The panic that stopped the destructor fails the test, unless the destructor
                    // caught it.
                    report.fail(format!(
                        "a destructor exceeded the step bound ({max_steps}) while it was being dropped at the end of \
                         the execution"
                    ));
                    Self::with(|state| state.abandon_teardown());
                    TEARDOWN_IGNORES_PANICS.set(true);
                }
                TeardownJob::Done => {
                    // What the tasks still hold goes last, as each task (see `drop_task_leftovers`).
                    // Destructors can spawn tasks, which are torn down too.
                    if !Self::drop_task_leftovers(&mut report) {
                        break;
                    }
                }
            }
        }

        // The tasks' labels, and tags that no task holds, while `EXECUTION_STATE` is set. Their
        // destructors may look up tasks: formatting a `TaskId` looks up its name, say. In the order
        // the tasks were created, so that the destructors run in the same order every time.
        let tags = TASK_ID_TO_TAGS.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
        let mut tags = tags.into_iter().collect::<Vec<_>>();
        tags.sort_unstable_by_key(|&(id, _)| id);
        for (_, tag) in tags {
            report.catch(move || drop(tag));
        }
        let labels = LABELS.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
        let mut labels = labels.into_iter().collect::<Vec<_>>();
        labels.sort_unstable_by_key(|&(id, _)| id);
        for (_, labels) in labels {
            report.catch(move || drop(labels));
        }
        let tasks = Self::with(|state| std::mem::take(&mut state.tasks));
        report.catch(move || drop(tasks));

        Self::with(|state| {
            state.teardown = None;
            #[cfg(debug_assertions)]
            {
                state.has_cleaned_up = true;
            }
        });
        drop(report_panics_again);
        report.finish()
    }

    /// Choose what execution teardown does next (see `tear_down`).
    fn next_teardown_job(&mut self, plan: &mut TeardownPlan) -> TeardownJob {
        let teardown = self.teardown();
        let kind = teardown.kind;
        // Once a destructor has exceeded the step bound, which fails the test, the rest of the
        // execution is abandoned: the tasks that are left are leaked, as destructors that spawn tasks
        // whose destructors spawn tasks, say, would go on forever.
        let exceeded_step_bound = teardown.exceeded_step_bound;
        if let Some(max_steps) = exceeded_step_bound.filter(|_| !kind.abandons()) {
            return TeardownJob::Abandon(max_steps);
        }
        let abandoned = exceeded_step_bound.is_some();
        if abandoned && !plan.suspended.is_empty() {
            return TeardownJob::Stuck;
        }

        // A task that was unwinding a panic when the execution stopped finishes unwinding it first.
        if let Some(id) = plan.unwinding_task.take() {
            return TeardownJob::FinishUnwinding(id);
        }

        // A cancelled task that blocked, and has been woken since, goes first.
        if let Some(i) = plan
            .suspended
            .iter()
            .position(|&(id, blocked)| blocked && self.get(id).runnable())
        {
            plan.idle_yields = 0;
            return TeardownJob::OnOwnStack(plan.suspended.remove(i).0, Resumption::Resume);
        }

        // Then the tasks that teardown hasn't looked at yet, in the order they were created.
        let leaks = UNGRACEFUL_SHUTDOWN_CONFIG.get().continuation_function_behavior.leaks();
        let (drop_functions, drop_parked_futures) = match kind {
            _ if abandoned => (false, false),
            TeardownKind::Finished => (true, true),
            TeardownKind::Stopped => (true, false),
            TeardownKind::Failed => (!leaks, !leaks),
        };
        while let Some(task) = self.tasks.get(plan.next_task) {
            plan.next_task += 1;
            let id = task.id();
            if task.finished() {
                // A future task's future can set task-local values after the task dropped its own
                // ones (see `Wrapper::finish`).
                if task.has_locals() {
                    return TeardownJob::TaskLocals(id);
                }
            } else if task.never_ran() {
                plan.idle_yields = 0;
                return if drop_functions {
                    TeardownJob::OnOwnStack(id, Resumption::Cancel)
                } else {
                    TeardownJob::LeakFunction(id)
                };
            } else if drop_parked_futures && task.parked_between_polls() {
                plan.idle_yields = 0;
                return TeardownJob::OnOwnStack(id, Resumption::Cancel);
            } else {
                // The task is in the middle of user code.
                plan.stacks.push_back(id);
            }
        }

        // A cancelled task that yielded runs again, in turn. While stacks are waiting to be unwound,
        // for a number of turns only, if no task finishes or blocks meanwhile (see
        // `TEARDOWN_YIELD_ROUNDS`).
        let unwinds_wait = kind == TeardownKind::Finished && !abandoned && !plan.stacks.is_empty();
        let yielded = |&(id, blocked): &(TaskId, bool)| !blocked && self.get(id).runnable();
        let num_yielded = plan.suspended.iter().filter(|task| yielded(task)).count();
        if num_yielded > 0 && (!unwinds_wait || plan.idle_yields < self.teardown_yield_budget(num_yielded)) {
            plan.idle_yields += 1;
            let i = plan.suspended.iter().position(yielded).unwrap();
            return TeardownJob::OnOwnStack(plan.suspended.remove(i).0, Resumption::Resume);
        }

        // A finished execution's tasks in the middle of `poll` are unwound now, once no other task
        // can make progress.
        if unwinds_wait {
            plan.idle_yields = 0;
            return TeardownJob::Unwind(plan.stacks.pop_front().unwrap());
        }

        // A static goes only once no cancelled task is left suspended, whose stack could still refer
        // to it.
        if !plan.suspended.is_empty() {
            return TeardownJob::Stuck;
        }

        if let Some(value) = self.storage.pop() {
            return TeardownJob::Static(value);
        }

        // An abandoned execution's stacks are leaked last, once the functions of the tasks that never
        // ran, which could borrow from them, are gone.
        if let Some(id) = plan.stacks.pop_front() {
            return TeardownJob::Leak(id);
        }

        TeardownJob::Done
    }

    /// How many turns in a row the tasks that yield get while stacks are waiting to be unwound (see
    /// `TEARDOWN_YIELD_ROUNDS`).
    fn teardown_yield_budget(&self, num_yielded: usize) -> usize {
        let rounds = TEARDOWN_YIELD_ROUNDS.saturating_mul(num_yielded);
        match self.teardown_step_bound() {
            Some(max_steps) => rounds.min(max_steps.saturating_sub(self.teardown().steps) / 4),
            None => rounds,
        }
    }

    /// Abandon the rest of a finished execution's teardown, which has failed (see `tear_down`).
    fn abandon_teardown(&mut self) {
        let teardown = self.teardown_mut();
        if teardown.kind == TeardownKind::Finished {
            teardown.kind = TeardownKind::Failed;
            self.current_task = TeardownKind::Failed.final_state();
        }
    }

    /// Resume a task on its own stack (see `Resumption`), until it finishes or switches out.
    fn run_on_own_stack(id: TaskId, resumption: Resumption, plan: &mut TeardownPlan, report: &mut TeardownReport) {
        let continuation = Self::with(|state| {
            state.begin_teardown_step(TeardownStep::Cancel(id));
            let task = state.get_mut(id);
            if resumption == Resumption::Cancel {
                // A parked future task is asleep, but it runs now, to drop its future.
                task.unblock();
            }
            task.continuation
                .clone()
                .expect("an unfinished task has a continuation")
        });

        Execution::enter_task_span();
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let mut continuation = continuation.borrow_mut();
            // Only now can the task switch out (see `maybe_yield_in_teardown`).
            Self::with(|state| state.teardown_mut().on_own_stack = true);
            if resumption == Resumption::Cancel {
                continuation.cancel()
            } else {
                continuation.resume()
            }
        }));
        Self::with(|state| state.teardown_mut().on_own_stack = false);
        Execution::exit_task_span(matches!(result, Ok(false)));
        drop(continuation);
        // A panic's payload goes while the step is under way, so its destructor runs as the task.
        let finished = result.unwrap_or_else(|payload| {
            report.panicked(payload);
            true
        });
        let blocked = Self::with(|state| {
            state.end_teardown_step();
            !state.get(id).runnable()
        });

        if finished {
            plan.idle_yields = 0;
            Self::finish_torn_down_task(id, report);
        } else {
            // A destructor blocked, or yielded (see `maybe_yield_in_teardown`).
            if blocked {
                plan.idle_yields = 0;
            }
            plan.suspended.push((id, blocked));
        }
    }

    /// Let a task that switched out while it unwound a panic, and is unwinding it still when a
    /// stopped execution stopped, finish unwinding it, on its own stack (see `tear_down`). The panic
    /// then fails the test. The task runs until it has caught the panic, if it does, but no further.
    /// A task that blocks while the panic unwinds, or exceeds the step bound, waits for tasks that no
    /// longer run, but cannot be stopped with a panic, as that would abort the process. It switches
    /// out instead (see `maybe_yield_in_teardown`), and fails the test. Then its stack is leaked,
    /// which leaves the thread panicking.
    fn finish_unwinding(id: TaskId, report: &mut TeardownReport) {
        let (continuation, blocked) = Self::with(|state| {
            state.begin_teardown_step(TeardownStep::FinishUnwinding(id));
            let task = state.get(id);
            let continuation = task
                .continuation
                .clone()
                .expect("an unfinished task has a continuation");
            (continuation, !task.runnable())
        });

        // A task that blocked already waits for a task that no longer runs.
        let result = if blocked {
            Ok(false)
        } else {
            Execution::enter_task_span();
            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                let mut continuation = continuation.borrow_mut();
                Self::with(|state| state.teardown_mut().on_own_stack = true);
                continuation.resume()
            }));
            Self::with(|state| state.teardown_mut().on_own_stack = false);
            Execution::exit_task_span(matches!(result, Ok(false)));
            result
        };
        drop(continuation);

        // On the executor's stack, `std::thread::panicking()` says whether the task is unwinding the
        // panic still (see `record_unwinding_task`).
        let stuck = matches!(result, Ok(false)) && std::thread::panicking();
        // A panic's payload goes while the step is under way, so its destructor runs as the task.
        let finished = result.unwrap_or_else(|payload| {
            report.late_panic(payload);
            true
        });
        if stuck {
            let (name, exceeded_step_bound) =
                Self::with(|state| (state.get(id).display_name(), state.teardown().exceeded_step_bound));
            let why = match exceeded_step_bound {
                Some(max_steps) => format!("exceeded the step bound ({max_steps})"),
                None => "blocked".into(),
            };
            report.fail(format!(
                "{name} was unwinding a panic when the execution stopped, and {why} before it had finished unwinding it"
            ));
        }
        Self::with(|state| state.end_teardown_step());

        if finished {
            Self::finish_torn_down_task(id, report);
        } else {
            // The task caught the panic, or is stuck.
            Self::leak_task(id, report);
        }
    }

    /// Unwind the stack of a finished execution's task that is in the middle of `poll` (see
    /// `tear_down`).
    fn unwind_task(id: TaskId, report: &mut TeardownReport) {
        let continuation = Self::with(|state| {
            state.begin_teardown_step(TeardownStep::Unwind(id));
            let task = state.get_mut(id);
            // The task may have been blocked, but it runs now, to drop what is on its stack.
            task.unblock();
            task.take_continuation()
        });
        let continuation = continuation.expect("an unfinished task has a continuation");

        // A panic while the stack unwinds aborts the process, so the panic hook reports every one.
        let ignoring_panics = TEARDOWN_IGNORES_PANICS.replace(false);
        // Among other things, this reinstates the task's default dispatcher, so that the guards on
        // its stack restore their priors in order as it unwinds (see `ParkedDefault`).
        Execution::enter_task_span();
        let result = panic::catch_unwind(panic::AssertUnwindSafe(move || {
            let continuation = Rc::try_unwrap(continuation).map_err(|_| ());
            let mut continuation = continuation.expect("teardown owns the continuation").into_inner();
            // Only now can the task switch, if it catches the unwind (see `maybe_yield_in_teardown`).
            Self::with(|state| state.teardown_mut().on_own_stack = true);
            continuation.unwind_stack();
        }));
        Self::with(|state| state.teardown_mut().on_own_stack = false);
        Execution::exit_task_span(false);
        TEARDOWN_IGNORES_PANICS.set(ignoring_panics);
        if let Err(payload) = result {
            report.panicked(payload);
        }
        Self::with(|state| {
            state.get_mut(id).lose_stack();
            state.end_teardown_step();
        });

        Self::finish_torn_down_task(id, report);
    }

    /// Leak the stack of an abandoned execution's task (see `tear_down`).
    fn leak_task(id: TaskId, report: &mut TeardownReport) {
        let (parked_default, continuation) = Self::with(|state| {
            let task = state.get_mut(id);
            task.lose_stack();
            (task.parked_default.take(), task.take_continuation())
        });
        // The parked default has to go before the task's stack does (see `ParkedDefault`).
        drop(parked_default);
        if let Some(continuation) = continuation {
            let continuation = Rc::try_unwrap(continuation).map_err(|_| ());
            let mut continuation = continuation.expect("teardown owns the continuation").into_inner();
            continuation.leak_stack();
        }

        Self::finish_torn_down_task(id, report);
    }

    /// Finish a task that execution teardown is done with, and drop the task-local values it left.
    fn finish_torn_down_task(id: TaskId, report: &mut TeardownReport) {
        crate::annotations::record_teardown_step(id);
        crate::annotations::record_task_terminated();
        Self::with(|state| {
            let task = state.get_mut(id);
            task.finish();
            // A task that joins this one can go on (see `JoinHandle::join`).
            if let Some(waiter) = task.take_waiter() {
                if !state.get(waiter).finished() {
                    state.get_mut(waiter).unblock();
                }
            }
        });
        Self::drop_leftover_task_locals(id, report);
    }

    /// Drop the task-local values that a task left, as the task, once its stack is gone (see
    /// `tear_down` and `Task::stand_in`).
    fn drop_leftover_task_locals(id: TaskId, report: &mut TeardownReport) {
        let state_before = Self::with(|state| {
            if !state.get(id).has_locals() {
                return None;
            }
            state.begin_teardown_step(TeardownStep::Leftovers(id));
            Some(state.get_mut(id).stand_in())
        });
        let Some(state_before) = state_before else {
            return;
        };

        // See `pop_local` for why this loop looks slightly funky.
        while let Some(local) = Self::with(|state| state.get_mut(id).pop_local()) {
            report.catch(move || drop(local));
        }

        Self::with(|state| {
            state.get_mut(id).stop_standing_in(state_before);
            state.end_teardown_step();
        });
    }

    /// Drop what the tasks still hold whose destructors may run user code, at the end of teardown:
    /// their spans, which a `tracing` subscriber sees close, and their tags. Each task's go as the
    /// task (see `Task::stand_in`). Returns whether there was anything to drop.
    fn drop_task_leftovers(report: &mut TeardownReport) -> bool {
        let ids = Self::with(|state| {
            TASK_ID_TO_TAGS.with(|tags| {
                let tags = tags.borrow();
                let has_tag = |id| !tags.is_empty() && tags.contains_key(&id);
                let tasks = state
                    .tasks
                    .iter()
                    .filter(|task| task.has_leftovers() || has_tag(task.id()));
                tasks.map(Task::id).collect::<Vec<_>>()
            })
        });
        for &id in &ids {
            let (state_before, leftovers) = Self::with(|state| {
                state.begin_teardown_step(TeardownStep::Leftovers(id));
                // `current::set_tag_for_task` keeps a reference to the tag too, which can be the last.
                let tag = TASK_ID_TO_TAGS.with(|tags| tags.borrow_mut().remove(&id));
                let task = state.get_mut(id);
                (task.stand_in(), (task.take_leftovers(), tag))
            });
            report.catch(move || drop(leftovers));
            Self::with(|state| {
                state.get_mut(id).stop_standing_in(state_before);
                state.end_teardown_step();
            });
        }
        !ids.is_empty()
    }

    /// Drop one of the execution's statics, as the main thread (see `tear_down`).
    fn drop_static(value: Box<dyn Any>, report: &mut TeardownReport) {
        let main_thread = TaskId(0);
        let state_before = Self::with(|state| {
            state.begin_teardown_step(TeardownStep::Static);
            state.get_mut(main_thread).stand_in()
        });

        report.catch(move || drop(value));
        // The static's destructor may have set task-local values of the main thread's, which go with
        // it.
        while let Some(local) = Self::with(|state| state.get_mut(main_thread).pop_local()) {
            report.catch(move || drop(local));
        }

        Self::with(|state| {
            state.get_mut(main_thread).stop_standing_in(state_before);
            state.end_teardown_step();
        });
    }

    /// Make `step` what execution teardown is doing, with its task as the current task (see
    /// `tear_down`).
    fn begin_teardown_step(&mut self, step: TeardownStep) {
        let id = match step {
            TeardownStep::Cancel(id)
            | TeardownStep::Unwind(id)
            | TeardownStep::FinishUnwinding(id)
            | TeardownStep::Leftovers(id) => id,
            // The main thread stands in for statics.
            TeardownStep::Static => TaskId(0),
            TeardownStep::Idle => unreachable!("teardown is always doing something during a step"),
        };
        self.current_task = ScheduledTask::Some(id);
        self.teardown_mut().step = step;
        TEARDOWN_STEP_PANICS.set(0);
        crate::annotations::record_teardown_step(id);
    }

    /// End the step that `begin_teardown_step` began, if one is under way.
    fn end_teardown_step(&mut self) {
        let teardown = self.teardown_mut();
        teardown.step = TeardownStep::Idle;
        self.current_task = teardown.kind.final_state();
    }

    fn teardown(&self) -> &Teardown {
        self.teardown.as_ref().expect("the execution is being torn down")
    }

    fn teardown_mut(&mut self) -> &mut Teardown {
        self.teardown.as_mut().expect("the execution is being torn down")
    }

    /// Whether the panic hook is to stay silent about a panic with `payload`, as execution teardown
    /// ignores the panics it catches now (see `tear_down`). A panic in a destructor while another
    /// panic unwinds aborts the process. So the hook reports the panic that says so, and any panic
    /// after the first in a step, or in what `TeardownReport::catch` drops, which may cause that.
    pub(crate) fn teardown_ignores_panic(payload: &dyn Any) -> bool {
        let panics = TEARDOWN_STEP_PANICS.get().saturating_add(1);
        TEARDOWN_STEP_PANICS.set(panics);
        let aborts = payload.downcast_ref::<&str>() == Some(&"panic in a destructor during cleanup");
        TEARDOWN_IGNORES_PANICS.get() && panics == 1 && !aborts
    }

    /// Whether execution teardown is unwinding the current task's stack (see `tear_down`). That is
    /// not a panic, although `std::thread::panicking()` says otherwise, so it shouldn't poison a
    /// lock, say.
    pub fn unwinding_for_teardown() -> bool {
        Self::try_with(|state| {
            state
                .teardown
                .as_ref()
                .is_some_and(|teardown| matches!(teardown.step, TeardownStep::Unwind(_)))
        })
        .unwrap_or(false)
    }

    /// Drop the current task's task-local values, as a task does when it finishes. A destructor can
    /// set another task-local value, which is dropped too (see `Task::pop_local`).
    pub fn drop_task_locals() {
        // See `pop_local` for why this loop looks slightly funky.
        while let Some(local) = Self::with(|state| state.current_mut().pop_local()) {
            tracing::trace!("dropping task-local value {:p}", local);
            drop(local);
        }
    }

    /// Stop a destructor that execution teardown cannot let go on (see `maybe_yield_in_teardown`).
    #[cold]
    fn stop_destructor(stall: TeardownStall) -> ! {
        if std::thread::panicking() {
            // This aborts the process, so report it in any case.
            TEARDOWN_IGNORES_PANICS.set(false);
        }
        let (name, step) = Self::with(|state| (state.current().display_name(), state.teardown().step));
        let who = match step {
            TeardownStep::Static => format!("A static's destructor, dropped as {name},"),
            TeardownStep::Leftovers(_) => format!("A destructor of something that {name} left behind"),
            _ => name,
        };
        match stall {
            TeardownStall::Blocked => {
                let reason = if std::thread::panicking() || matches!(step, TeardownStep::Unwind(_)) {
                    "while unwinding a stack, which cannot be suspended"
                } else {
                    "outside any task's stack, where it cannot wait"
                };
                panic!(
                    "{who} blocked while it was being dropped at the end of the execution, {reason}. A destructor made a \
                     blocking call, such as locking a mutex that another task holds, receiving from an empty channel, \
                     joining a task, or `block_on` on a future that cannot complete."
                )
            }
            TeardownStall::ExceededStepBound(max_steps) => panic!(
                "{who} exceeded the step bound ({max_steps}) while it was being dropped at the end of the execution. A \
                 destructor may be spinning, waiting for something that no longer happens: once the execution is over, \
                 only destructors run."
            ),
        }
    }

    /// Determine whether the execution has finished.
    pub fn is_finished(&self) -> bool {
        self.current_task == ScheduledTask::Stopped || self.current_task == ScheduledTask::Finished
    }

    /// Invoke the scheduler to decide which task to schedule next. Returns true if the chosen task
    /// is different from the currently running task, indicating that the current task should yield
    /// its execution.
    pub fn maybe_yield() -> bool {
        let decision = Self::with(|state| {
            if state.teardown.is_some() {
                return state.maybe_yield_in_teardown();
            }

            if std::thread::panicking() {
                if !state.switched_out_unwinding {
                    state.record_unwinding_task();
                }
                return Ok(true);
            }
            if state.switched_out_unwinding {
                // No task is unwinding a panic, as the tasks share the OS thread.
                state.forget_unwinding_task();
            }

            debug_assert!(
                matches!(state.current_task, ScheduledTask::Some(_)) && state.next_task == ScheduledTask::None,
                "we're inside a task and scheduler should not yet have run"
            );

            let result = state.schedule();
            // If scheduling failed, yield so that the outer scheduling loop can handle it.
            if result.is_err() {
                return Ok(true);
            }

            // If the next task is the same as the current one, we can skip the context switch
            // and just advance to the next task immediately.
            if state.current_task == state.next_task {
                state.advance_to_next_task();
                Ok(false)
            } else {
                Ok(true)
            }
        });
        // Panic outside of `Self::with`, so the panic hook can name the task.
        decision.unwrap_or_else(|stall| Self::stop_destructor(stall))
    }

    /// The current task switches out while it unwinds a panic, and no task was unwinding one when it
    /// was resumed (see `switched_out_unwinding`), so the panic is its own. (`std::thread::panicking()`
    /// can't tell which task panics: tasks share the OS thread. Between steps, on the executor's
    /// stack, it says exactly whether a task that switched out is unwinding a panic.) Until the task
    /// has finished unwinding the panic, the execution waits for it, even if it is detached.
    /// Otherwise the execution could end before the task resumes, and its panic would be lost. There
    /// is one such task at most: while it is unwinding, a task that is resumed can't tell a panic of
    /// its own from it.
    #[cold]
    fn record_unwinding_task(&mut self) {
        debug_assert!(self.unwinding_task.is_none());
        self.switched_out_unwinding = true;
        if let Some(me) = self.current_task.id() {
            let detached = std::mem::replace(&mut self.get_mut(me).detached, false);
            self.unwinding_task = Some((me, detached));
        }
    }

    /// No task is unwinding a panic any more: the task that was recorded doing so (see
    /// `record_unwinding_task`) is detached again if it was before.
    #[cold]
    fn forget_unwinding_task(&mut self) {
        self.switched_out_unwinding = false;
        if let Some((task, detached)) = self.unwinding_task.take() {
            self.get_mut(task).detached = detached;
        }
    }

    /// Detach a task, so that the execution doesn't wait for it to finish, as when its `JoinHandle`
    /// is dropped. A task that is unwinding a panic is detached once it has finished unwinding (see
    /// `record_unwinding_task`).
    pub fn detach(&mut self, id: TaskId) {
        match &mut self.unwinding_task {
            Some((task, detached)) if *task == id => *detached = true,
            _ => self.get_mut(id).detach(),
        }
    }

    /// `maybe_yield` during execution teardown, which schedules nothing (see `tear_down`). Each
    /// scheduling point counts as a step, which the step bound limits as in a running execution.
    /// A task that teardown runs on its own stack switches out when it blocks or yields, so that
    /// teardown can run others. Anywhere else, or while a panic unwinds, nothing can be suspended,
    /// so a destructor that blocks is stopped with a panic. A task that finishes unwinding a panic
    /// switches out instead, and as soon as it has caught the panic (see `finish_unwinding`).
    #[cold]
    fn maybe_yield_in_teardown(&mut self) -> Result<bool, TeardownStall> {
        // Destructors do what tasks do, so `current::context_switches` counts their scheduling points
        // too.
        self.context_switches += 1;
        let panicking = std::thread::panicking();
        let yielded = std::mem::take(&mut self.has_yielded);
        let max_steps = self.teardown_step_bound();

        let teardown = self.teardown_mut();
        teardown.steps += 1;
        let finishing_unwind = teardown.on_own_stack && matches!(teardown.step, TeardownStep::FinishUnwinding(_));
        if let Some(max_steps) = max_steps {
            // Stopping a destructor while a panic unwinds aborts the process, so give the unwind,
            // likely that of the panic that stopped the destructor, some more steps.
            let bound = if panicking && !finishing_unwind {
                max_steps.saturating_mul(2)
            } else {
                max_steps
            };
            if teardown.steps > bound {
                teardown.exceeded_step_bound = Some(max_steps);
                if finishing_unwind {
                    return Ok(true);
                }
                return Err(TeardownStall::ExceededStepBound(max_steps));
            }
        }
        let on_own_stack = teardown.on_own_stack && !panicking;
        if on_own_stack && matches!(teardown.step, TeardownStep::Unwind(_)) {
            // The task caught the unwind of its stack, which starts again once the task switches
            // (see `Coroutine::force_unwind`). It has to run for that.
            self.current_mut().unblock();
            return Ok(true);
        }
        if finishing_unwind {
            // Once no panic is unwinding, the task has caught its panic.
            return Ok(!panicking || !self.current().runnable());
        }

        // Between steps (dropping the payload of a panic, say), no task runs that could switch out.
        let Some(runnable) = self.try_current().map(Task::runnable) else {
            return Ok(false);
        };
        if !runnable && !on_own_stack {
            // A destructor blocked the task where it cannot wait. Make it runnable again, so that the
            // unwind of the panic that stops the destructor doesn't stall too.
            self.current_mut().unblock();
            return Err(TeardownStall::Blocked);
        }
        Ok(on_own_stack && (!runnable || yielded))
    }

    /// The step bound for the destructors that execution teardown runs, if there is one (see
    /// `maybe_yield_in_teardown`).
    fn teardown_step_bound(&self) -> Option<usize> {
        match self.config.max_steps {
            MaxSteps::FailAfter(max_steps) => Some(max_steps),
            // A small bound that stops an execution is not meant to fail a test, and teardown has no
            // way to stop. So destructors get at least as many steps as an execution does by default.
            MaxSteps::ContinueAfter(max_steps) => Some(max_steps.max(DEFAULT_MAX_STEPS)),
            MaxSteps::None => None,
        }
    }

    /// Tell the scheduler that the next context switch is an explicit yield requested by the
    /// current task. Some schedulers use this as a hint to influence scheduling.
    pub fn request_yield() {
        Self::with(|state| {
            state.has_yielded = true;
        });
    }

    /// Check whether Shuttle's own `Drop` handlers should skip their bookkeeping, and early exit,
    /// because the execution has stopped, and so is being abandoned (see `tear_down`).
    ///
    /// We also stop if we are currently panicking (e.g., perhaps we're unwinding the stack for a
    /// panic triggered while someone held a Mutex, and so are executing the Drop handler for
    /// MutexGuard). This avoids calling back into the scheduler during a panic, because the state
    /// may be poisoned or otherwise invalid.
    ///
    /// While a finished execution is torn down, though, destructors run as in a running execution,
    /// also while a panic unwinds: teardown doesn't call the scheduler. A failed execution is
    /// abandoned like a stopped one.
    pub fn should_stop() -> bool {
        if std::thread::panicking() {
            // The state may be borrowed, so don't insist on it.
            return Self::try_with(|s| s.stops(true)).unwrap_or(true);
        }
        Self::with(|s| s.stops(false))
    }

    /// `should_stop`, given whether the current task is panicking.
    pub(crate) fn stops(&self, panicking: bool) -> bool {
        match &self.teardown {
            Some(teardown) => teardown.kind.abandons(),
            None => {
                panicking || {
                    assert_ne!(self.current_task, ScheduledTask::Finished);
                    self.current_task == ScheduledTask::Stopped
                }
            }
        }
    }

    /// Whether the execution stopped or failed, so that the stacks of its unfinished tasks are
    /// leaked rather than unwound (see `tear_down`).
    pub(crate) fn execution_stopped() -> bool {
        Self::try_with(|state| match &state.teardown {
            Some(teardown) => teardown.kind.abandons(),
            None => state.current_task == ScheduledTask::Stopped,
        })
        .unwrap_or(false)
    }

    /// Generate some diagnostic information used when persisting failures.
    ///
    /// Because this method may be called from a panic hook, it must not panic.
    pub fn failing_task() -> String {
        Self::try_with(|state| {
            if let Some(task) = state.try_current() {
                let name = task.display_name();
                match state.teardown.as_ref().map(|teardown| teardown.step) {
                    None | Some(TeardownStep::Idle) => name,
                    Some(TeardownStep::Static) => format!("{name} (dropping a static at the end of the execution)"),
                    Some(TeardownStep::FinishUnwinding(_)) => {
                        format!("{name} (unwinding a panic at the end of the execution)")
                    }
                    Some(_) => format!("{name} (being dropped at the end of the execution)"),
                }
            } else if let Some(name) = &state.failed_task {
                // The task that failed the execution, which has been torn down since.
                name.clone()
            } else {
                "<unknown>".into()
            }
        })
        .unwrap_or_else(|e| format!("Tried to get ExecutionState, but got the following error: {e:?}"))
    }

    /// Generate a random u64 from the current scheduler and return it.
    #[inline]
    pub fn next_u64() -> u64 {
        Self::with(|state| {
            if let Some(teardown) = &mut state.teardown {
                // Teardown doesn't extend the schedule (see `Teardown::rng`).
                return teardown.next_u64();
            }
            CurrentSchedule::push_random();
            state.scheduler.borrow_mut().next_u64()
        })
    }

    pub fn current(&self) -> &Task {
        self.get(self.current_task.id().expect("there is no current task"))
    }

    pub fn current_mut(&mut self) -> &mut Task {
        self.get_mut(self.current_task.id().expect("there is no current task"))
    }

    pub fn try_current(&self) -> Option<&Task> {
        self.try_get(self.current_task.id()?)
    }

    pub fn get(&self, id: TaskId) -> &Task {
        self.try_get(id).unwrap()
    }

    /// Register a newly created task. Task ids are handed out sequentially as `tasks.len()`, so the
    /// new id is always greater than every existing one and `live_tasks` stays sorted.
    fn add_task(&mut self, task: Task) {
        debug_assert!(self.live_tasks.last().is_none_or(|last| *last < task.id()));
        self.live_tasks.push(task.id());
        self.tasks.push(task);
    }

    /// Mark the task as finished and drop it from the set of live tasks.
    fn finish_task(&mut self, task_id: TaskId) {
        self.get_mut(task_id).finish();
        let idx = self
            .live_tasks
            .binary_search(&task_id)
            .expect("finished task must be live");
        self.live_tasks.remove(idx);
        if self.unwinding_task.is_some_and(|(task, _)| task == task_id) {
            self.unwinding_task = None;
        }
    }

    /// Mark the current task as finished and drop it from the set of live tasks.
    fn finish_current_task(&mut self) {
        self.finish_task(self.current_task.id().unwrap());
    }

    pub fn get_mut(&mut self, id: TaskId) -> &mut Task {
        self.tasks.get_mut(id.0).unwrap()
    }

    pub fn try_get(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(id.0)
    }

    pub fn try_get_mut(&mut self, id: TaskId) -> Option<&mut Task> {
        self.tasks.get_mut(id.0)
    }

    /// Whether the execution is being torn down (see `tear_down`).
    pub fn in_cleanup(&self) -> bool {
        self.teardown.is_some()
    }

    pub fn context_switches() -> usize {
        Self::with(|state| state.context_switches)
    }

    #[track_caller]
    pub fn new_resource_signature(resource_type: ResourceType) -> ResourceSignature {
        ExecutionState::with(|s| s.current_mut().signature.new_resource(resource_type))
    }

    pub fn get_storage<K: Into<StorageKey>, T: 'static>(&self, key: K) -> Option<&T> {
        self.storage
            .get(key.into())
            .map(|result| result.expect("global storage is never destructed"))
    }

    pub fn init_storage<K: Into<StorageKey>, T: 'static>(&mut self, key: K, value: T) {
        self.storage.init(key.into(), value);
    }

    pub fn get_clock(&self, id: TaskId) -> &VectorClock {
        &self.tasks.get(id.0).unwrap().clock
    }

    pub fn get_clock_mut(&mut self, id: TaskId) -> &mut VectorClock {
        &mut self.tasks.get_mut(id.0).unwrap().clock
    }

    /// Increment the current thread's clock entry and update its clock with the one provided.
    pub fn update_clock(&mut self, clock: &VectorClock) {
        let task = self.current_mut();
        task.clock.increment(task.id);
        task.clock.update(clock);
    }

    /// Increment the current thread's clock and return a shared reference to it
    pub fn increment_clock(&mut self) -> &VectorClock {
        let task = self.current_mut();
        task.clock.increment(task.id);
        &task.clock
    }

    /// Increment the current thread's clock and return a mutable reference to it
    pub fn increment_clock_mut(&mut self) -> &mut VectorClock {
        let task = self.current_mut();
        task.clock.increment(task.id);
        &mut task.clock
    }

    /// Set the number of steps used, with respect to the step bound, to 0 (see
    /// `current::reset_step_count`). During execution teardown, these are the steps of destructors
    /// (see `maybe_yield_in_teardown`).
    pub(crate) fn reset_step_count(&mut self) {
        match &mut self.teardown {
            Some(teardown) => teardown.steps = 0,
            None => self.steps_reset_at = CurrentSchedule::len(),
        }
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
                    return Err(StepError::StepBoundExceeded(max_steps));
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

        // The loop below only looks at `live_tasks`, so a task missing from that set would silently
        // never be scheduled. Check that direction of the invariant here; the loop itself checks the
        // other direction (that no finished task is still in the set).
        debug_assert!(
            self.tasks
                .iter()
                .filter(|task| !task.finished() && task.runnable())
                .all(|task| self.live_tasks.binary_search(&task.id()).is_ok()),
            "live_tasks is missing a runnable unfinished task"
        );

        for &task_id in &self.live_tasks {
            let task = &self.tasks[task_id.0];
            debug_assert!(!task.finished());
            unfinished_attached |= !task.detached;
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
    pub fn set_tag_for_current_task(tag: Arc<dyn Tag>) -> Option<Arc<dyn Tag>> {
        ExecutionState::with(|s| s.current_mut().set_tag(tag))
    }

    #[allow(deprecated)]
    fn get_tag_or_default_for_current_task(&self) -> Option<Arc<dyn Tag>> {
        self.try_current().and_then(|current| current.get_tag())
    }

    #[allow(deprecated)]
    pub fn get_tag_for_current_task() -> Option<Arc<dyn Tag>> {
        ExecutionState::with(|s| s.get_tag_or_default_for_current_task())
    }

    #[allow(deprecated)]
    pub fn set_tag_for_task(task: TaskId, tag: Arc<dyn Tag>) -> Option<Arc<dyn Tag>> {
        ExecutionState::with(|s| s.get_mut(task).set_tag(tag))
    }
}

#[cfg(debug_assertions)]
impl Drop for ExecutionState {
    fn drop(&mut self) {
        assert!(self.has_cleaned_up || std::thread::panicking());
    }
}

/// Whether the running task's default dispatcher might not be the execution's.
fn task_may_have_own_default(state: &ExecutionState) -> bool {
    use tracing::level_filters::LevelFilter;

    // No dispatcher enables anything, so whichever one is the default makes no difference. (If that
    // changes while the task is switched out, other tasks see its default, as before defaults were
    // parked.)
    if LevelFilter::current() == LevelFilter::OFF {
        return false;
    }
    tracing::dispatcher::get_default(|current| {
        if let Some(global) = state.top_level_dispatch_global_ptr {
            // `get_default` hands out the global default from where it lives, and anything else from
            // this thread's scoped default. So if it hands out the global default, the task has no
            // default of its own. Parking otherwise is sometimes unnecessary but always safe,
            // including if `tracing` ever handed out the global default from elsewhere.
            !std::ptr::eq(current, global)
        } else if current.is::<NoSubscriber>() {
            // Nothing is recorded either way if the execution's default is `Dispatch::none()` too.
            !state.top_level_dispatch.is::<NoSubscriber>()
        } else {
            // A task can only have a default of its own through a scoped default. `get_default` hands
            // nested calls `Dispatch::none()` only while some thread has one; otherwise they get the
            // global default, which is then `current` and everyone's default.
            tracing::dispatcher::get_default(|nested| nested.is::<NoSubscriber>())
        }
    })
}
