use crate::runtime::task::{clock::VectorClock, Task, TaskId};
use crate::scheduler::data::random::RandomDataSource;
use crate::scheduler::data::DataSource;
use crate::scheduler::serialization::deserialize_schedule;
use crate::scheduler::{Schedule, ScheduleStep, Scheduler};
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;
use tracing::trace;

/// A scheduler that can replay a chosen schedule deserialized from a string.
#[derive(Debug)]
pub struct ReplayScheduler {
    schedule: Schedule,
    steps: usize,
    steps_skipped: usize,
    started: bool,
    allow_incomplete: bool,
    data_source: RandomDataSource,
    target_clock: Option<VectorClock>,
}

impl ReplayScheduler {
    /// Construct a new ReplayScheduler from configuration.
    pub fn from_config(config: &config::Config) -> Self {
        if let Ok(path) = config.get_string("scheduler.schedule_file") {
            Self::new_from_file(path).expect("failed to load schedule from file")
        } else {
            let schedule = config
                .get_string("scheduler.schedule")
                .expect("replay scheduler requires 'scheduler.schedule' or 'scheduler.schedule_file' config");
            Self::new_from_encoded(&schedule)
        }
    }

    /// Given an encoded schedule, construct a new [`ReplayScheduler`] that will execute threads in
    /// the order specified in the schedule.
    pub fn new_from_encoded(encoded_schedule: &str) -> Self {
        let schedule = deserialize_schedule(encoded_schedule).expect("invalid schedule");
        Self::new_from_schedule(schedule)
    }

    /// Given a file containing a schedule, construct a new [`ReplayScheduler`] that will execute
    /// threads in the order specified in the schedule.
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
            steps_skipped: 0,
            started: false,
            allow_incomplete: false,
            data_source,
            target_clock: None,
        }
    }

    /// Set flag to allow early termination of a schedule
    pub fn set_allow_incomplete(&mut self) {
        self.allow_incomplete = true;
    }

    /// Set a clock of the failure to be reproduced. Events which are not
    /// causally related to this clock (i.e., events concurrent to the failure)
    /// will not be scheduled.
    pub fn set_target_clock(&mut self, clock: impl Into<VectorClock>) {
        self.target_clock = Some(clock.into());
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

    fn next_task(&mut self, runnable: &[&Task], _current: Option<TaskId>, _is_yielding: bool) -> Option<TaskId> {
        loop {
            if self.steps >= self.schedule.steps.len() {
                assert!(self.allow_incomplete, "schedule ended early");
                return None;
            }
            match self.schedule.steps[self.steps] {
                ScheduleStep::Random => {
                    panic!("expected context switch but next schedule step is random choice");
                }
                ScheduleStep::Task(next) => {
                    if let Some(task) = runnable.iter().find(|t| t.id() == next) {
                        self.steps += 1;
                        if let Some(target_clock) = &self.target_clock {
                            if task.clock <= *target_clock {
                                // The target event causally depends on this
                                // event, so we schedule it.
                                return Some(next);
                            } else {
                                // The target event is concurrent with this
                                // event, so it is irrelevant to the replay.
                                // At this point, we also need to skip over
                                // any random steps made by the thread that
                                // would have been scheduled.
                                let mut skipped = 1;
                                while let Some(ScheduleStep::Random) = self.schedule.steps.get(self.steps) {
                                    skipped += 1;
                                    self.steps += 1;
                                    self.data_source.next_u64();
                                }
                                trace!(
                                    "skipped step of replayed sequence due to causality, followed by {} random steps",
                                    skipped - 1
                                );
                                self.steps_skipped += skipped;
                                continue;
                            }
                        } else {
                            return Some(next);
                        }
                    } else {
                        assert!(
                            self.allow_incomplete,
                            "scheduled task is not runnable, expected to run {next:?}, but choices were {runnable:?}"
                        );
                        return None;
                    }
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
