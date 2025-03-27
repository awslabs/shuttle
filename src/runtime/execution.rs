use crate::runtime::failure::{init_panic_hook, persist_failure, persist_task_failure};
use crate::runtime::storage::{StorageKey, StorageMap};
use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::labels::Labels;
use crate::runtime::task::{ChildLabelFn, Task, TaskId, TaskName, DEFAULT_INLINE_TASKS};
use crate::runtime::thread::continuation::PooledContinuation;
use crate::scheduler::{Schedule, Scheduler};
use crate::thread::thread_fn;
use crate::{Config, MaxSteps};
use scoped_tls::scoped_thread_local;
use smallvec::SmallVec;
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Debug;
use std::future::Future;
use std::panic;
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
            ExecutionState::spawn_thread(
                move || thread_fn(f, Default::default()),
                config.stack_size,
                Some("main-thread".to_string()),
                Some(VectorClock::new()),
            );

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
                    // The scheduler decided we're finished, so there are either no runnable tasks,
                    // or all runnable tasks are detached and there are no unfinished attached
                    // tasks. Therefore, it's a deadlock if there are unfinished attached tasks.
                    if state.tasks.iter().any(|t| !t.finished() && !t.detached) {
                        let blocked_tasks = state
                            .tasks
                            .iter()
                            .filter(|t| !t.finished())
                            .map(|t| {
                                format!(
                                    "{} (task {:?}{}{})",
                                    t.name().unwrap_or_else(|| "<unknown>".to_string()),
                                    t.id(),
                                    if t.detached { ", detached" } else { "" },
                                    if t.sleeping() { ", pending future" } else { "" },
                                )
                            })
                            .collect::<Vec<_>>();
                        NextStep::Failure(
                            format!("deadlock! blocked tasks: [{}]", blocked_tasks.join(", ")),
                            state.current_schedule.clone(),
                        )
                    } else {
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
                            state.current().step_span.record("i", state.current_schedule.len());
                        }
                    });
                });

                let result = panic::catch_unwind(panic::AssertUnwindSafe(|| continuation.borrow_mut().resume()));

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

                result
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
                crate::annotations::record_task_terminated();
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
    // the schedule length last time `reset_stop_bound()` was called
    pub(crate) steps_reset_at: usize,

    // static values for the current execution
    storage: StorageMap,

    scheduler: Rc<RefCell<dyn Scheduler>>,
    pub(crate) current_schedule: Schedule,

    in_cleanup: bool,

    #[cfg(debug_assertions)]
    has_cleaned_up: bool,

    // The `Span` which the `ExecutionState` was created under. Will be the parent of all `Task` `Span`s
    pub(crate) top_level_span: Span,
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
            steps_reset_at: 0,
            storage: StorageMap::new(),
            scheduler,
            current_schedule: initial_schedule,
            in_cleanup: false,
            #[cfg(debug_assertions)]
            has_cleaned_up: false,
            top_level_span: tracing::Span::current(),
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
        Self::try_with(f).expect("Shuttle internal error: cannot access ExecutionState. are you trying to access a Shuttle primitive from outside a Shuttle test?")
    }

    /// Like `with`, but returns None instead of panicking if there is no current ExecutionState or
    /// if the current ExecutionState is already borrowed.
    #[inline]
    pub(crate) fn try_with<F, T>(f: F) -> Option<T>
    where
        F: FnOnce(&mut ExecutionState) -> T,
    {
        if EXECUTION_STATE.is_set() {
            EXECUTION_STATE.with(|cell| {
                if let Ok(mut state) = cell.try_borrow_mut() {
                    Some(f(&mut state))
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

    /// Spawn a new task for a future. This doesn't create a yield point; the caller should do that
    /// if it wants to give the new task a chance to run immediately.
    pub(crate) fn spawn_future<F>(future: F, stack_size: usize, name: Option<String>) -> TaskId
    where
        F: Future<Output = ()> + 'static,
    {
        let task_id = Self::with(|state| {
            let schedule_len = state.current_schedule.len();
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
                state.try_current().map(|t| t.id()),
            );

            state.tasks.push(task);

            task_id
        });
        crate::annotations::record_task_created(task_id, true);
        task_id
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

            let schedule_len = state.current_schedule.len();

            let task = Task::from_closure(
                f,
                stack_size,
                task_id,
                name,
                clock,
                parent_span_id,
                schedule_len,
                tag,
                state.try_current().map(|t| t.id()),
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
            (std::mem::replace(&mut state.tasks, SmallVec::new()), state.current_task)
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

    pub(crate) fn in_cleanup(&self) -> bool {
        self.in_cleanup
    }

    pub(crate) fn context_switches() -> usize {
        Self::with(|state| state.context_switches)
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
        self.current_schedule.len() - self.steps_reset_at >= max_steps
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
            MaxSteps::FailAfter(max_steps) if self.is_step_bound_exceeded(max_steps) => {
                let msg = format!(
                    "exceeded max_steps bound {}. this might be caused by an unfair schedule (e.g., a spin loop)?",
                    max_steps
                );
                return Err(msg);
            }
            MaxSteps::ContinueAfter(max_steps) if self.is_step_bound_exceeded(max_steps) => {
                self.next_task = ScheduledTask::Stopped;
                return Ok(());
            }
            _ => {}
        }

        let mut unfinished_attached = false;
        let mut runnable = self
            .tasks
            .iter()
            .inspect(|t| unfinished_attached = unfinished_attached || (!t.finished() && !t.detached))
            .filter(|t| t.runnable())
            .map(|t| t.id)
            .collect::<SmallVec<[_; DEFAULT_INLINE_TASKS]>>();

        // We should finish execution when either
        // (1) There are no runnable tasks, or
        // (2) All runnable tasks have been detached AND there are no unfinished attached tasks
        // If there are some unfinished attached tasks and all runnable tasks are detached, we must
        // run some detached task to give them a chance to unblock some unfinished attached task.
        if runnable.is_empty() || (!unfinished_attached && runnable.iter().all(|id| self.get(*id).detached)) {
            self.next_task = ScheduledTask::Finished;
            return Ok(());
        }

        // Some blocked tasks can be woken up spuriously, even though the condition the task is
        // blocked on hasn't happened yet. We'll add such tasks to the list of runnable tasks, but
        // they won't contribute to the check on `runnable.is_empty()` if the only runnable tasks
        // are ones that are waiting for a potential spurious wakeup, it should still be treated as
        // a deadlock since there's no guarantee that spurious wakeups will ever occur.
        runnable.extend(self.tasks.iter().filter(|t| t.can_spuriously_wakeup()).map(|t| t.id));

        let is_yielding = std::mem::replace(&mut self.has_yielded, false);

        let runnable_tasks = runnable
            .iter()
            .map(|id| self.tasks.get(id.0).unwrap())
            .collect::<SmallVec<[&Task; DEFAULT_INLINE_TASKS]>>();
        self.next_task = self
            .scheduler
            .borrow_mut()
            .next_task(&runnable_tasks, self.current_task.id(), is_yielding)
            .map(ScheduledTask::Some)
            .unwrap_or(ScheduledTask::Stopped);
        drop(runnable_tasks);

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

        // Tracing this `in_scope` is purely a matter of taste. We do it because
        // 1) It is an action taken by the scheduler, and should thus be traced under the scheduler's span
        // 2) It creates a visual separation of scheduling decisions and `Task`-induced tracing.
        // Note that there is a case to be made for not `in_scope`-ing it, as that makes seeing the context
        // of the context switch clearer.
        //
        // Note also that changing this trace! statement requires changing the test `basic::labels::test_tracing_with_label_fn`
        // which relies on this trace reporting the `runnable` tasks.
        self.top_level_span
            .in_scope(|| trace!(i=self.current_schedule.len(), next_task=?self.next_task, ?runnable));

        Ok(())
    }

    /// Set the next task as the current task
    fn advance_to_next_task(&mut self) {
        debug_assert_ne!(self.next_task, ScheduledTask::None);
        self.current_task = self.next_task.take();

        if let ScheduledTask::Some(tid) = self.current_task {
            self.current_schedule.push_task(tid);
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
