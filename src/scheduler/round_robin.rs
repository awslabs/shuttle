use crate::runtime::task_id::TaskId;
use crate::scheduler::Scheduler;

/// A round robin scheduler that chooses the next available runnable task at each context switch.
#[derive(Debug, Default)]
pub struct RoundRobinScheduler {
    iterations: usize,
}

impl RoundRobinScheduler {
    /// Construct a new `RoundRobinScheduler` that will execute the test only once, scheduling its
    /// tasks in a round-robin fashion.
    pub fn new() -> Self {
        Self { iterations: 0 }
    }
}

impl Scheduler for RoundRobinScheduler {
    fn new_execution(&mut self) -> bool {
        if self.iterations == 0 {
            self.iterations += 1;
            true
        } else {
            false
        }
    }

    fn next_task(&mut self, runnable: &[TaskId], current: Option<TaskId>) -> TaskId {
        if current.is_none() {
            return *runnable.first().unwrap();
        }
        let current = current.unwrap();

        *runnable
            .iter()
            .find(|t| **t > current)
            .unwrap_or_else(|| runnable.first().unwrap())
    }
}
