use crate::runtime::task::serialization::deserialize_schedule;
use crate::runtime::task::TaskId;
use crate::scheduler::Scheduler;

/// A scheduler that can replay a chosen schedule deserialized from a string.
#[derive(Debug, Default)]
pub struct ReplayScheduler {
    schedule: Vec<TaskId>,
    steps: usize,
    started: bool,
}

impl ReplayScheduler {
    /// Construct a new `RoundRobinScheduler` that will execute the test only once, scheduling its
    /// tasks in a round-robin fashion.
    pub fn new(encoded_schedule: &str) -> Self {
        Self {
            schedule: deserialize_schedule(encoded_schedule).expect("invalid schedule"),
            steps: 0,
            started: false,
        }
    }
}

impl Scheduler for ReplayScheduler {
    fn new_execution(&mut self) -> bool {
        if self.started {
            false
        } else {
            self.started = true;
            true
        }
    }

    fn next_task(&mut self, runnable: &[TaskId], _current: Option<TaskId>) -> TaskId {
        assert!(self.steps < self.schedule.len(), "schedule ended early");
        let next = self.schedule[self.steps];
        assert!(runnable.contains(&next), "scheduled task is not runnable");
        self.steps += 1;
        next
    }
}
