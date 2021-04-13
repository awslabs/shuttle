use crate::runtime::task::TaskId;
use crate::scheduler::data::random::RandomDataSource;
use crate::scheduler::data::DataSource;
use crate::scheduler::serialization::deserialize_schedule;
use crate::scheduler::{Schedule, ScheduleStep, Scheduler};
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;

/// A scheduler that can replay a chosen schedule deserialized from a string.
#[derive(Debug)]
pub struct ReplayScheduler {
    schedule: Schedule,
    steps: usize,
    started: bool,
    allow_incomplete: bool,
    data_source: RandomDataSource,
}

impl ReplayScheduler {
    /// Given an encoded schedule, construct a new [`ReplayScheduler`] that will execute threads in
    /// the order specified in the schedule.
    pub fn new_from_encoded(encoded_schedule: &str) -> Self {
        let schedule = deserialize_schedule(encoded_schedule).expect("invalid schedule");
        Self::new_from_schedule(schedule)
    }

    /// Given a file containing a schedule, construct a new [`ReplayScheduler`] that will execute
    /// threads in the order epseicied in the schedule.
    pub fn new_from_file<P: AsRef<Path>>(path: P) -> Result<Self, std::io::Error> {
        let mut file = OpenOptions::new().read(true).open(path)?;
        let mut encoded_schedule = String::new();
        file.read_to_string(&mut encoded_schedule)?;
        Ok(Self::new_from_encoded(&encoded_schedule))
    }

    /// Given a schedule, construct a new [`ReplayScheduler`] that will execute threads in the order
    /// specified in the schedule.
    pub fn new_from_schedule(schedule: Schedule) -> Self {
        let data_source = RandomDataSource::initialize(schedule.seed);

        Self {
            schedule,
            steps: 0,
            started: false,
            allow_incomplete: false,
            data_source,
        }
    }

    /// Set flag to allow early termination of a schedule
    pub fn set_allow_incomplete(&mut self) {
        self.allow_incomplete = true;
    }
}

impl Scheduler for ReplayScheduler {
    fn new_execution(&mut self) -> Option<Schedule> {
        if self.started {
            None
        } else {
            self.started = true;
            Some(Schedule::new(self.data_source.reinitialize()))
        }
    }

    fn next_task(&mut self, runnable: &[TaskId], _current: Option<TaskId>, _is_yielding: bool) -> Option<TaskId> {
        if self.steps >= self.schedule.steps.len() {
            assert!(self.allow_incomplete, "schedule ended early");
            return None;
        }
        match self.schedule.steps[self.steps] {
            ScheduleStep::Random => {
                panic!("expected context switch but next schedule step is random choice");
            }
            ScheduleStep::Task(next) => {
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
    }

    fn next_u64(&mut self) -> u64 {
        match self.schedule.steps[self.steps] {
            ScheduleStep::Random => {
                self.steps += 1;
                self.data_source.next_u64()
            }
            ScheduleStep::Task(_) => {
                panic!("expected random choice but next schedule step is context switch");
            }
        }
    }
}
