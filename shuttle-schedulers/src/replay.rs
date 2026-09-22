use crate::RandomScheduler;
use shuttle_engine::runtime::task::{clock::VectorClock, Task, TaskId};
use shuttle_engine::scheduler::data::random::RandomDataSource;
use shuttle_engine::scheduler::data::DataSource;
use shuttle_engine::scheduler::serialization::deserialize_schedule;
use shuttle_engine::scheduler::{Schedule, ScheduleStep, Scheduler};
use std::fmt::{self, Debug};
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;
use tracing::{info, trace};

/// If this environment variable is set (to any value), a [`ReplayScheduler`] continues the execution
/// after the end of the recorded schedule, as if
/// [`set_continue_after_schedule`](ReplayScheduler::set_continue_after_schedule) had been called.
const CONTINUE_AFTER_SCHEDULE: &str = "SHUTTLE_CONTINUE_AFTER_SCHEDULE";

/// What a [`ReplayScheduler`] does once it can no longer follow the recorded schedule, either
/// because the schedule ran out of steps or because the task it says to run next is not runnable.
enum OnScheduleEnd {
    /// Panic. The recorded schedule is expected to cover the entire execution.
    Panic,
    /// End the execution where the schedule ends
    /// (see [`ReplayScheduler::set_allow_incomplete`]).
    Stop,
    /// Hand the rest of the execution over to another scheduler
    /// (see [`ReplayScheduler::set_continue_after_schedule`]).
    Continue(Box<dyn Scheduler + Send>),
}

// `Scheduler` is not `Debug`, so the boxed continuation scheduler cannot be printed.
impl Debug for OnScheduleEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Panic => f.write_str("Panic"),
            Self::Stop => f.write_str("Stop"),
            Self::Continue(_) => f.write_str("Continue(<scheduler>)"),
        }
    }
}

/// A scheduler that can replay a chosen schedule deserialized from a string.
#[derive(Debug)]
pub struct ReplayScheduler {
    schedule: Schedule,
    steps: usize,
    steps_skipped: usize,
    started: bool,
    on_schedule_end: OnScheduleEnd,
    data_source: RandomDataSource,
    target_clock: Option<VectorClock>,
}

impl ReplayScheduler {
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
    ///
    /// If the `SHUTTLE_CONTINUE_AFTER_SCHEDULE` environment variable is set, the scheduler is
    /// configured as if [`set_continue_after_schedule`](Self::set_continue_after_schedule) had been
    /// called, so that replays driven by a harness (which owns the call to this constructor) can
    /// continue past the end of the schedule too.
    pub fn new_from_schedule(schedule: Schedule) -> Self {
        let data_source = RandomDataSource::initialize(schedule.seed);

        let mut scheduler = Self {
            schedule,
            steps: 0,
            steps_skipped: 0,
            started: false,
            on_schedule_end: OnScheduleEnd::Panic,
            data_source,
            target_clock: None,
        };

        if std::env::var(CONTINUE_AFTER_SCHEDULE).is_ok() {
            info!(
                "{CONTINUE_AFTER_SCHEDULE} is set: continuing after the schedule under a random scheduler seeded with {}",
                scheduler.schedule.seed
            );
            scheduler.set_continue_after_schedule();
        }

        scheduler
    }

    /// Set flag to allow early termination of a schedule
    pub fn set_allow_incomplete(&mut self) {
        self.on_schedule_end = OnScheduleEnd::Stop;
    }

    /// Continue the execution after the recorded schedule has been replayed, rather than ending it,
    /// by handing the remaining scheduling decisions to a [`RandomScheduler`] seeded with the
    /// recorded schedule's own seed.
    ///
    /// This is useful for exploring what a program does *after* a prefix of interest: record a
    /// schedule, cut it short (or change the program so that the tail of the schedule no longer
    /// applies), and replay it to reach the interesting state, from where the execution continues
    /// as an ordinary randomized one. Because the continuation scheduler is seeded from the
    /// schedule, replaying the same schedule twice takes the same continuation; and because the
    /// runner records the steps it actually took, a failure found in the continuation is reported
    /// as a schedule that replays the whole execution on its own.
    ///
    /// Note that a continued execution has no reason to terminate on its own, so it is bounded by
    /// [`Config::max_steps`](shuttle_engine::Config::max_steps) (a million steps by default) rather
    /// than by the length of the schedule.
    ///
    /// See [`set_continue_after_schedule_with`](Self::set_continue_after_schedule_with) to continue
    /// under a different scheduler.
    pub fn set_continue_after_schedule(&mut self) {
        self.set_continue_after_schedule_with(RandomScheduler::new_from_seed(self.schedule.seed, 1));
    }

    /// Like [`set_continue_after_schedule`](Self::set_continue_after_schedule), but continues the
    /// execution under `scheduler` instead of a default [`RandomScheduler`].
    ///
    /// `scheduler` only chooses which task to run: the random values handed to the program continue
    /// to come from the replayed schedule's seed, so that the execution stays reproducible from the
    /// schedule alone.
    ///
    /// `scheduler` is never told that an execution is starting, because the execution it joins is
    /// already underway — and because a `new_execution` call belongs to a search rather than a
    /// replay, so it would reseed the scheduler and produce its own failure reports. A scheduler
    /// that sets up per-execution state in [`Scheduler::new_execution`] (like
    /// [`PctScheduler`](crate::PctScheduler)) therefore continues in whatever state its constructor
    /// left it in; call `new_execution` on it yourself first if that state matters.
    pub fn set_continue_after_schedule_with(&mut self, scheduler: impl Scheduler + Send + 'static) {
        self.on_schedule_end = OnScheduleEnd::Continue(Box::new(scheduler));
    }

    /// Set a clock of the failure to be reproduced. Events which are not
    /// causally related to this clock (i.e., events concurrent to the failure)
    /// will not be scheduled.
    pub fn set_target_clock(&mut self, clock: impl Into<VectorClock>) {
        self.target_clock = Some(clock.into());
    }

    /// Abandon the rest of the recorded schedule, because the program has diverged from it: our
    /// position in the schedule no longer corresponds to the state of the program, so a later step
    /// that happens to be runnable would only match by coincidence.
    fn abandon_schedule(&mut self) {
        trace!(
            "schedule diverged after {} steps; the continuation scheduler takes over",
            self.steps
        );
        self.steps = self.schedule.steps.len();
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

    fn next_task(&mut self, runnable: &[&Task], current: Option<TaskId>, is_yielding: bool) -> Option<TaskId> {
        loop {
            if self.steps >= self.schedule.steps.len() {
                return match &mut self.on_schedule_end {
                    OnScheduleEnd::Panic => panic!("schedule ended early"),
                    OnScheduleEnd::Stop => None,
                    OnScheduleEnd::Continue(scheduler) => scheduler.next_task(runnable, current, is_yielding),
                };
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
                    } else if matches!(self.on_schedule_end, OnScheduleEnd::Continue(_)) {
                        // Hand over to the continuation scheduler on the next turn around the loop.
                        self.abandon_schedule();
                        continue;
                    } else {
                        assert!(
                            matches!(self.on_schedule_end, OnScheduleEnd::Stop),
                            "scheduled task is not runnable, expected to run {next:?}, but choices were {runnable:?}"
                        );
                        return None;
                    }
                }
            }
        }
    }

    fn next_u64(&mut self) -> u64 {
        // Random values always come from our own data source, even once the continuation scheduler
        // is driving the execution, so that the schedule the runner records replays on its own.
        if self.steps >= self.schedule.steps.len() {
            assert!(
                matches!(self.on_schedule_end, OnScheduleEnd::Continue(_)),
                "schedule ended early on a random choice"
            );
            return self.data_source.next_u64();
        }
        match self.schedule.steps[self.steps] {
            ScheduleStep::Random => {
                self.steps += 1;
                self.data_source.next_u64()
            }
            ScheduleStep::Task(_) => {
                if matches!(self.on_schedule_end, OnScheduleEnd::Continue(_)) {
                    // The program asked for a random value where the schedule expects a context
                    // switch, so it has diverged from the schedule.
                    self.abandon_schedule();
                    return self.data_source.next_u64();
                }
                panic!("expected random choice but next schedule step is context switch");
            }
        }
    }
}
