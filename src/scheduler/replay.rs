use crate::runtime::task::TaskId;
use crate::scheduler::serialization::deserialize_schedule;
use crate::scheduler::{Schedule, Scheduler};

/// A scheduler that can replay a chosen schedule deserialized from a string.
#[derive(Debug, Default)]
pub struct ReplayScheduler {
    schedule: Schedule,
    steps: usize,
    started: bool,
    allow_incomplete: bool,
}

impl ReplayScheduler {
    /// Given an encoded schedule, construct a new `ReplayScheduler` that will execute threads
    /// in the order specified in the schedule.
    pub fn new_from_encoded(encoded_schedule: &str) -> Self {
        Self {
            schedule: deserialize_schedule(encoded_schedule).expect("invalid schedule"),
            steps: 0,
            started: false,
            allow_incomplete: false,
        }
    }

    /// Given an unencoded schedule, construct a new `ReplayScheduler` that will execute threads
    /// in the order specified in the schedule.
    pub fn new_from_schedule(schedule: Schedule) -> Self {
        Self {
            schedule,
            steps: 0,
            started: false,
            allow_incomplete: false,
        }
    }

    /// Set flag to allow early termination of a schedule
    pub fn set_allow_incomplete(&mut self) {
        self.allow_incomplete = true;
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
        if self.steps >= self.schedule.len() {
            assert!(self.allow_incomplete, "schedule ended early");
            return None;
        }
        let next = self.schedule[self.steps];
        if !runnable.contains(&next) {
            assert!(
                self.allow_incomplete,
                "scheduled task is not runnable, expected to run {:?}, but choices were {:?}",
                next, runnable
            );
            None
        } else {
            self.steps += 1;
            Some(next)
        }
    }
}
