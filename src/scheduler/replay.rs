use crate::runtime::task::TaskId;
use crate::scheduler::serialization::deserialize_schedule;
use crate::scheduler::Scheduler;

/// A scheduler that can replay a chosen schedule deserialized from a string.
#[derive(Debug, Default)]
pub struct ReplayScheduler {
    schedule: Vec<TaskId>,
    steps: usize,
    started: bool,
}

impl ReplayScheduler {
    /// Given an encoded schedule, construct a new `ReplayScheduler` that will execute threads
    /// in the order specified in the schedule.
    pub fn new_from_encoded(encoded_schedule: &str) -> Self {
        Self {
            schedule: deserialize_schedule(encoded_schedule).expect("invalid schedule"),
            steps: 0,
            started: false,
        }
    }

    /// Given an unencoded schedule, construct a new `ReplayScheduler` that will execute threads
    /// in the order specified in the schedule.
    pub fn new_from_schedule(schedule: Vec<usize>) -> Self {
        let schedule: Vec<TaskId> = schedule.into_iter().map(TaskId::from).collect();
        Self {
            schedule,
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

    fn next_task(&mut self, runnable: &[TaskId], _current: Option<TaskId>) -> Option<TaskId> {
        assert!(self.steps < self.schedule.len(), "schedule ended early");
        let next = self.schedule[self.steps];
        assert!(
            runnable.contains(&next),
            "scheduled task is not runnable, expected to run {:?}, but choices were {:?}",
            next,
            runnable
        );
        self.steps += 1;
        Some(next)
    }
}
