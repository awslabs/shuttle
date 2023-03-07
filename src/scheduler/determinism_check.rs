use std::collections::HashSet;
use std::iter::FromIterator;

use crate::runtime::task::TaskId;
use crate::scheduler::{Schedule, ScheduleStep, Scheduler};

/// A `ScheduleRecord` can be used to record both the step
/// taken and all runnable tasks
#[derive(Clone, Debug, PartialEq, Eq)]
struct ScheduleRecord {
    step: ScheduleStep,
    runnable_tasks: HashSet<TaskId>,
}

impl ScheduleRecord {
    /// Create a record step from the chosen task and options
    fn new(step: ScheduleStep, runnable_tasks: &[TaskId]) -> Self {
        let runnable_tasks = runnable_tasks.iter().cloned().collect::<HashSet<_>>();
        Self { step, runnable_tasks }
    }
}

/// A `DeterminismCheckScheduler` checks whether a given program is deterministic
/// by wrapping an inner `Scheduler` and, for each schedule generated by that scheduler,
/// replaying the schedule a chosen number of times. On each replay, we check that the
/// schedule is still valid and that the set of runnable tasks is the same at each step.
/// Violations of these checks mean that the program is not deterministic.
#[derive(Debug)]
pub struct DeterminismCheckScheduler<S: ?Sized + Scheduler> {
    inner: Box<S>,
    iterations: usize,
    inner_iterations: usize,
    recording: bool,
    original_schedule: Vec<ScheduleRecord>,
    current_step: usize,
}

impl<S: Scheduler> DeterminismCheckScheduler<S> {
    /// Create a new `DeterminismCheckScheduler` by wrapping the given `Scheduler` implementation.
    pub fn new(inner_iterations: usize, inner: S) -> Self {
        assert!(inner_iterations > 0);
        Self {
            inner: Box::new(inner),
            iterations: 0,
            inner_iterations,
            original_schedule: Vec::new(),
            recording: false,
            current_step: 0,
        }
    }
}

impl<S: Scheduler> Scheduler for DeterminismCheckScheduler<S> {
    fn new_execution(&mut self) -> Option<Schedule> {
        if self.iterations % self.inner_iterations == 0 {
            // Start a new recording
            self.recording = true;
            self.original_schedule.clear();
        } else {
            // Create a new execution to test against prior recording
            self.recording = false;

            if self.current_step != self.original_schedule.len() {
                panic!("possible nondeterminism: current execution ended earlier than expected (expected length {} but ended after {})", self.original_schedule.len(), self.current_step);
            }
        }

        self.current_step = 0;
        self.iterations += 1;

        self.inner.new_execution()
    }

    fn next_task(
        &mut self,
        runnable_tasks: &[TaskId],
        current_task: Option<TaskId>,
        is_yielding: bool,
    ) -> Option<TaskId> {
        if self.recording {
            // Recording a schedule
            let choice = self.inner.next_task(runnable_tasks, current_task, is_yielding).unwrap();
            self.original_schedule
                .push(ScheduleRecord::new(ScheduleStep::Task(choice), runnable_tasks));

            self.current_step += 1;

            Some(choice)
        } else {
            // Determine whether the state is the same

            if self.current_step >= self.original_schedule.len() {
                panic!(
                    "possible nondeterminism: current execution should have ended after {} steps, whereas current step count is {}",
                    self.original_schedule.len(),
                    self.current_step
                );
            }

            let expected = &self.original_schedule[self.current_step];
            let actual_options: HashSet<TaskId> = HashSet::from_iter(runnable_tasks.iter().cloned());

            match expected.step {
                ScheduleStep::Task(id) => {
                    if !expected.runnable_tasks.contains(&id) {
                        panic!("possible nondeterminism: expected next task is not runnable (expected to run {:?} but runnable tasks were {:?}", id, actual_options);
                    }

                    if expected.runnable_tasks != actual_options {
                        panic!("possible nondeterminism: set of runnable tasks is different than expected (expected {:?} but got {:?})", expected.runnable_tasks, actual_options);
                    }

                    self.current_step += 1;

                    Some(id)
                }
                ScheduleStep::Random => {
                    panic!("possible nondeterminism: next step was context switch, but recording expected random number generation")
                }
            }
        }
    }

    fn next_u64(&mut self) -> u64 {
        if self.recording {
            // Recording a schedule
            self.original_schedule
                .push(ScheduleRecord::new(ScheduleStep::Random, &Vec::new()));
        } else {
            // Check that random step is recorded here
            if let ScheduleStep::Task(_) = self.original_schedule[self.current_step].step {
                panic!("possible nondeterminism: next step was random number generation, but recording expected context switch");
            }
        }

        self.current_step += 1;
        self.inner.next_u64()
    }
}