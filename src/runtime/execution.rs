// To use the scoped version of the `generator` API, we'd need a way to store each continuation's
// `Scope` object locally. Normally that would be TLS but those use platform threads, so aren't
// aware of `generator` threads. Instead we just fall back to using the unscoped API.
#![allow(deprecated)]

use crate::scheduler::Scheduler;
use generator::{Generator, Gn};
use scoped_tls::scoped_thread_local;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};

// A note on terminology: we have competing notions of threads floating around. Here's the
// convention for disambiguating them:
// * A "thread" is the user-level unit of concurrency. User code creates threads, passes data
//   between them, etc. There is no notion of "thread" inside this runtime. There are other possible
//   user-level units of concurrency, like Futures, but we currently only support threads.
// * A "task" is this runtime's reflection of a user-level unit of concurrency. A task has a state
//   like "blocked", "runnable", etc. Scheduling algorithms take as input the state of all tasks
//   and decide which task should execute next. A context switch is when one task stops executing
//   and another begins.
// * A "continuation" is the low-level implementation of concurrency. Each task has a corresponding
//   continuation. When a scheduler decides to execute a task, we resume its continuation and run
//   until that continuation yields, which happens when its task decides it might want to context
//   switch.

// TODO make this a parameter
const MAX_TASKS: usize = 4;

// We use this scoped TLS to smuggle the ExecutionState, which is not 'static, across continuations.
// The scoping handles the lifetime issue. Note that TLS is not aware of our generator threads; this
// is one shared cell across all threads in the execution.
scoped_thread_local! {
    static EXECUTION_STATE: RefCell<ExecutionState>
}

/// An execution is the top-level data structure for a single run of a function under test. It holds
/// the raw continuations used for each task, and the "state" of the execution (which tasks are
/// blocked, finished, etc).
///
/// The "state" needs to be mutably accessible from each of the continuations. The Execution
/// arranges that whenever it resumes a continuation, the `EXECUTION_STATE` static holds a reference
/// to the current execution state.
pub(crate) struct Execution<'a> {
    continuations: Vec<Continuation>,
    // invariant: a task must not hold a borrow of this cell when it calls Execution::switch
    state: RefCell<ExecutionState>,
    // can't make this a type parameter because we have static methods we want to call
    scheduler: &'a mut Box<dyn Scheduler>,
}

impl<'a> Execution<'a> {
    /// Construct a new execution that will use the given scheduler. The execution should then be
    /// invoked via its `run` method, which takes as input the closure for task 0.
    // TODO this is where we'd pass in config for max_tasks etc
    pub(crate) fn new(scheduler: &'a mut Box<dyn Scheduler>) -> Self {
        // We pre-spawn the maximum number of continuations we might need, to avoid needing to spawn
        // them during the execution (which would require more mutable state than we'd like). Each
        // continuation has a lifecycle:
        // 1. the first time it resumes, it immediately yields a WaitingForFunction message. This
        //    first invocation happens immediately, within this loop, to prime the continuation for
        //    use
        // 2. the second time it resumes, it expects to receive a Function message containing the
        //    function it will run. It stores that function away, and yields a Ready message. This
        //    second invocation happens when the function under test spawns a new task.
        // 3. the third time it resumes, it expects to receive a Resume message, and then invokes
        //    the function it received earlier. This invocation, and all future invocations, are
        //    triggered by the scheduler. The function can yield at any time when it wants to invoke
        //    a possible context switch.
        // 4. when the function for this continuation finished, the continuations yields a final
        //    Finished message. Resuming the continuation after that point is an error.
        // If a continuation never ends up being used during the execution, the teardown will send
        // it a Shutdown message and resume it at step (2) above. This isn't necessary for
        // correctness, but precludes some bugs in which a continuation is somehow left unfinished.
        let mut continuations = vec![];
        for _ in 0..MAX_TASKS {
            let mut gen = Gn::new_opt(0x8000, move || {
                let f = generator::yield_(ContinuationOutput::WaitingForFunction).unwrap();

                match f {
                    ContinuationInput::Function(f) => {
                        generator::yield_with(ContinuationOutput::Ready);

                        f();

                        ContinuationOutput::Finished
                    }
                    ContinuationInput::Shutdown => ContinuationOutput::Shutdown,
                    _ => panic!("unexpected continuation input"),
                }
            });

            // Resume once to prime the continuation for use. It will yield waiting to receive a
            // function to run.
            let r = gen.resume();
            assert!(matches!(r, Some(ContinuationOutput::WaitingForFunction)));

            continuations.push(Continuation(gen));
        }

        let state = RefCell::new(ExecutionState::new());
        Self {
            continuations,
            state,
            scheduler,
        }
    }
}

impl Execution<'_> {
    /// Run a function to be tested, taking control of scheduling it and any tasks it might spawn.
    /// This function runs until `f` and all tasks spawned by `f` have terminated.
    pub(crate) fn run<F>(mut self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        // Install `f` as the first task to be spawned
        self.state.borrow_mut().enqueue_spawn(f);

        loop {
            let mut state = self.state.borrow_mut();

            // Actually spawn any tasks that are ready to spawn. This leaves them in Runnable
            // state, but their inner functions have not yet been invoked.
            while let Some(f) = state.pending_spawns.pop_front() {
                let tid = TaskId(state.tasks.len());
                let task = Task {
                    id: tid,
                    state: TaskState::Runnable,
                    waiter: None,
                };
                state.tasks.push(task);

                // Send the function into the corresponding continuation, which prepares the task
                // to be run, but doesn't actually run it yet.
                let c = self.continuations.get_mut(tid.0).unwrap();
                c.0.set_para(ContinuationInput::Function(f));
                let r = c.0.resume();
                assert!(matches!(r, Some(ContinuationOutput::Ready)));
            }

            let runnable = state
                .tasks
                .iter()
                .filter(|t| t.state == TaskState::Runnable)
                .map(|t| t.id)
                .collect::<Vec<_>>();

            if runnable.is_empty() {
                let task_states = state.tasks.iter().map(|t| (t.id, t.state)).collect::<Vec<_>>();
                if task_states.iter().any(|(_, s)| *s == TaskState::Blocked) {
                    panic!("deadlock: {:?}", task_states);
                }
                assert!(state.tasks.iter().all(|t| t.finished()));
                break;
            }

            let to_run = self.scheduler.next_task(&runnable, state.current_task);

            state.current_task = Some(to_run);
            drop(state);

            // Resume the continuation for the task we scheduled
            // TODO if performance is an issue, we might decouple this from the scheduler, so that
            // TODO we can remain inside a continuation if the scheduler chose the same task
            let continuation = self.continuations.get_mut(to_run.0).unwrap();
            let ret = EXECUTION_STATE.set(&self.state, || {
                continuation.set_para(ContinuationInput::Resume);
                continuation.resume().unwrap()
            });

            let mut state = self.state.borrow_mut();
            match ret {
                ContinuationOutput::Finished => {
                    state.current_mut().state = TaskState::Finished;
                }
                ContinuationOutput::Yielded => (),
                _ => panic!("unexpected continuation output"),
            }
        }

        // The program is now finished. These are just sanity checks for cleanup: every task should
        // have reached Finished state, and every unused continuation should shutdown cleanly
        let state = self.state.into_inner();
        for (c, thd) in self.continuations.drain(..state.tasks.len()).zip(state.tasks.iter()) {
            assert_eq!(thd.state, TaskState::Finished);
            assert!(c.is_done());
        }
        for mut c in self.continuations.drain(..) {
            c.set_para(ContinuationInput::Shutdown);
            let r = c.resume().unwrap();
            assert!(matches!(r, ContinuationOutput::Shutdown));
            assert!(c.is_done());
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
        Self::with_state(|state| state.enqueue_spawn(f))
    }

    /// Yield back to the scheduler to perform a (possible) context switch.
    pub(crate) fn switch() {
        let r = generator::yield_(ContinuationOutput::Yielded).unwrap();
        assert!(matches!(r, ContinuationInput::Resume));
    }

    /// A shortcut to get the current task ID
    pub(crate) fn me() -> TaskId {
        Self::with_state(|s| s.current().id())
    }
}

struct Continuation(Generator<'static, ContinuationInput, ContinuationOutput>);

impl Deref for Continuation {
    type Target = Generator<'static, ContinuationInput, ContinuationOutput>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Continuation {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
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
    // TODO I think this is guaranteed to be length <= 1 in our current Thread impl
    pending_spawns: VecDeque<Box<dyn FnOnce()>>,
}

impl ExecutionState {
    fn new() -> Self {
        Self {
            tasks: vec![],
            current_task: None,
            pending_spawns: VecDeque::new(),
        }
    }

    /// Spawn a new task.
    fn enqueue_spawn<F>(&mut self, f: F) -> TaskId
    where
        F: FnOnce() + Send + 'static,
    {
        let next_task_id = self.tasks.len() + self.pending_spawns.len();
        self.pending_spawns.push_back(Box::new(f));
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

/// A `Task` represents a user-level unit of concurrency (currently, that's just a thread). Each
/// task has an `id` that is unique within the execution, and a `state` reflecting whether the task
/// is runnable (enabled) or not.
pub(crate) struct Task {
    id: TaskId,
    state: TaskState,
    waiter: Option<TaskId>,
}

impl Task {
    pub(crate) fn id(&self) -> TaskId {
        self.id
    }

    pub(crate) fn finished(&self) -> bool {
        self.state == TaskState::Finished
    }

    pub(crate) fn blocked(&self) -> bool {
        self.state == TaskState::Blocked
    }

    pub(crate) fn waiter(&self) -> Option<TaskId> {
        self.waiter
    }

    pub(crate) fn block(&mut self) {
        assert_eq!(self.state, TaskState::Runnable);
        self.state = TaskState::Blocked;
    }

    pub(crate) fn unblock(&mut self) {
        assert_eq!(self.state, TaskState::Blocked);
        self.state = TaskState::Runnable;
    }

    /// Like `unblock` but the target doesn't have to be already blocked
    pub(crate) fn maybe_unblock(&mut self) {
        self.state = TaskState::Runnable;
    }

    /// Register a waiter for this thread to terminate. Returns a boolean indicating whether the
    /// waiter should block or not. If false, this task has already finished, and so the waiter need
    /// not block.
    pub(crate) fn wait_for(&mut self, waiter: TaskId) -> bool {
        assert!(self.waiter.is_none());
        if self.finished() {
            false
        } else {
            self.waiter = Some(waiter);
            true
        }
    }
}

#[derive(PartialEq, Eq, Hash, Clone, Copy, PartialOrd, Ord, Debug)]
pub struct TaskId(usize);

impl From<usize> for TaskId {
    fn from(id: usize) -> Self {
        TaskId(id)
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum TaskState {
    Runnable,
    Blocked,
    Finished,
}

/// Inputs that the executor can pass to a continuation.
pub(crate) enum ContinuationInput {
    Function(Box<dyn FnOnce()>),
    Resume,
    Shutdown,
}

/// Outputs that a continuation can return back to the executor.
#[derive(Debug)]
pub(crate) enum ContinuationOutput {
    WaitingForFunction,
    Ready,
    Yielded,
    Finished,
    Shutdown,
}
