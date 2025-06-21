//! Implementations of different scheduling strategies for concurrency testing.
use std::fmt::Debug;

mod annotation;
mod data;
mod dfs;
mod pct;
mod random;
mod replay;
mod round_robin;
mod uncontrolled_nondeterminism;

pub(crate) mod metrics;
pub(crate) mod serialization;

pub use crate::runtime::task::{Task, TaskId};

pub use annotation::AnnotationScheduler;
pub use data::{DataSource, RandomDataSource};
pub use dfs::DfsScheduler;
pub use pct::PctScheduler;
pub use random::RandomScheduler;
pub use replay::ReplayScheduler;
pub use round_robin::RoundRobinScheduler;
pub use uncontrolled_nondeterminism::UncontrolledNondeterminismCheckScheduler;

/// A `Schedule` determines the order in which tasks are to be executed
// TODO would be nice to make this generic in the type of `seed`, but for now all our seeds are u64s
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schedule {
    seed: u64,
    steps: Vec<ScheduleStep>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ScheduleStep {
    Task(TaskId),
    Random,
}

impl Schedule {
    /// Create a new empty `Schedule` that starts with the given random seed.
    pub fn new(seed: u64) -> Self {
        Self { seed, steps: vec![] }
    }

    /// Create a new `Schedule` that begins by scheduling the given tasks.
    pub fn new_from_task_ids<T>(seed: u64, task_ids: impl IntoIterator<Item = T>) -> Self
    where
        T: Into<TaskId>,
    {
        let steps = task_ids
            .into_iter()
            .map(|t| ScheduleStep::Task(t.into()))
            .collect::<Vec<_>>();
        Self { seed, steps }
    }

    /// Add the given task ID as the next step of the schedule.
    pub fn push_task(&mut self, task: TaskId) {
        self.steps.push(ScheduleStep::Task(task));
    }

    /// Add a choice of a random u64 value as the next step of the schedule.
    pub fn push_random(&mut self) {
        self.steps.push(ScheduleStep::Random);
    }

    /// Return the number of steps in the schedule.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Return true if the schedule is empty.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// A `Scheduler` is an oracle that decides the order in which to execute concurrent tasks and the
/// data to return to calls for random values.
///
/// The `Scheduler` lives across multiple executions of the test case, allowing it to retain some
/// state and strategically explore different schedules. At the start of each test execution, the
/// executor calls `new_execution()` to inform the scheduler that a new execution is starting. Then,
/// for each scheduling decision, the executor calls `next_task` to determine which task to run.
pub trait Scheduler {
    /// Inform the `Scheduler` that a new execution is about to begin. If this function returns
    /// None, the test will end rather than performing another execution. If it returns
    /// `Some(schedule)`, the returned `Schedule` can be used to initialize a `ReplayScheduler` for
    /// deterministic replay.
    fn new_execution(&mut self) -> Option<Schedule>;

    /// Decide which task to run next, given a list of runnable tasks and the currently running
    /// tasks. This method returns `Some(task)` where `task` is the runnable task to be executed
    /// next; it may also return `None`, indicating that the execution engine should stop exploring
    /// the current schedule.
    ///
    /// `is_yielding` is a hint to the scheduler that `current_task` has asked to yield (e.g.,
    /// during a spin loop) and should be deprioritized.
    ///
    /// The list of runnable tasks is guaranteed to be non-empty. If `current_task` is `None`, the
    /// execution has not yet begun.
    fn next_task(
        &mut self,
        runnable_tasks: &[&Task],
        current_task: Option<TaskId>,
        is_yielding: bool,
    ) -> Option<TaskId>;

    /// Choose the next u64 value to return to the currently running task.
    fn next_u64(&mut self) -> u64;
}

impl Scheduler for Box<dyn Scheduler + Send> {
    fn new_execution(&mut self) -> Option<Schedule> {
        self.as_mut().new_execution()
    }

    fn next_task(
        &mut self,
        runnable_tasks: &[&Task],
        current_task: Option<TaskId>,
        is_yielding: bool,
    ) -> Option<TaskId> {
        self.as_mut().next_task(runnable_tasks, current_task, is_yielding)
    }

    fn next_u64(&mut self) -> u64 {
        self.as_mut().next_u64()
    }
}
