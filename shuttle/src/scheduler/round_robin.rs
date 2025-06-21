use crate::runtime::task::{Task, TaskId};
use crate::scheduler::data::random::RandomDataSource;
use crate::scheduler::data::DataSource;
use crate::scheduler::{Schedule, Scheduler};

/// A round robin scheduler that chooses the next available runnable task at each context switch.
#[derive(Debug)]
pub struct RoundRobinScheduler {
    iterations: usize,
    max_iterations: usize,
    data_source: RandomDataSource,
}

impl RoundRobinScheduler {
    /// Construct a new `RoundRobinScheduler` that will execute the test up to max_iteration times, scheduling its
    /// tasks in a round-robin fashion.
    pub fn new(max_iterations: usize) -> Self {
        Self {
            iterations: 0,
            max_iterations,
            data_source: RandomDataSource::initialize(0),
        }
    }
}

impl Scheduler for RoundRobinScheduler {
    fn new_execution(&mut self) -> Option<Schedule> {
        if self.iterations < self.max_iterations {
            self.iterations += 1;
            Some(Schedule::new(self.data_source.reinitialize()))
        } else {
            None
        }
    }

    fn next_task(&mut self, runnable: &[&Task], current: Option<TaskId>, _is_yielding: bool) -> Option<TaskId> {
        if current.is_none() {
            return Some(runnable.first().unwrap().id());
        }
        let current = current.unwrap();

        Some(
            runnable
                .iter()
                .find(|t| t.id() > current)
                .unwrap_or_else(|| runnable.first().unwrap())
                .id(),
        )
    }

    fn next_u64(&mut self) -> u64 {
        self.data_source.next_u64()
    }
}

impl Default for RoundRobinScheduler {
    fn default() -> Self {
        Self::new(1)
    }
}
