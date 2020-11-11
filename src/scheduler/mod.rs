//! Implementations of different scheduling strategies for concurrency testing.
use crate::runtime::task::TaskId;
use std::fmt::Debug;
use std::ops::{Deref, DerefMut};

mod dfs;
mod pct;
mod random;
mod replay;
mod round_robin;

pub(crate) mod metrics;
pub(crate) mod serialization;

pub use dfs::DFSScheduler;
pub use pct::PCTScheduler;
pub use random::RandomScheduler;
pub use replay::ReplayScheduler;
pub use round_robin::RoundRobinScheduler;

/// A `Schedule` determines the order in which tasks are to be executed
#[derive(Clone, Debug, Default)]
pub struct Schedule(Vec<TaskId>);

impl Schedule {
    fn new() -> Self {
        Self(vec![])
    }
}

impl Deref for Schedule {
    type Target = Vec<TaskId>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Schedule {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<&[usize]> for Schedule {
    fn from(tasks: &[usize]) -> Self {
        Schedule(tasks.iter().map(|&n| TaskId::from(n)).collect())
    }
}

impl From<Vec<usize>> for Schedule {
    fn from(tasks: Vec<usize>) -> Self {
        Schedule(tasks.iter().map(|&n| TaskId::from(n)).collect())
    }
}

impl From<Schedule> for Vec<usize> {
    fn from(schedule: Schedule) -> Self {
        schedule.0.iter().map(|&t| usize::from(t)).collect()
    }
}

/// A `Scheduler` is an oracle that decides the order in which to execute concurrent tasks.
///
/// The`Scheduler` lives across multiple executions of the test case, allowing it to retain some
/// state and strategically explore different schedules. At the start of each test execution, the
/// executor calls `new_execution()` to inform the scheduler that a new execution is starting. Then,
/// for each scheduling decision, the executor calls `next_task` to determine which task to run.
// TODO need a way to a scheduler to terminate within an execution (e.g. max depth reached)?
pub trait Scheduler: Debug {
    /// Inform the `Scheduler` that a new execution is about to begin. If this function returns
    /// false, the test will end rather than performing another execution.
    fn new_execution(&mut self) -> bool;

    /// Decide which task to run next, given a list of runnable tasks and the currently running
    /// tasks. If `current_task` is `None`, the execution has not yet begun. The list of runnable
    /// tasks is guaranteed to be non-empty.  This method returns `Some(task)` where `task` is
    /// the runnable task to be executed next; it may also return `None`, indicating that the
    /// execution engine should stop exploring the current schedule.
    fn next_task(&mut self, runnable_tasks: &[TaskId], current_task: Option<TaskId>) -> Option<TaskId>;
}
