use std::collections::HashSet;
use std::iter::FromIterator;

use crate::runtime::task::TaskId;
use crate::scheduler::{Schedule, Scheduler,ScheduleStep, ScheduleRecord};

// #[derive(Clone, Debug, PartialEq, Eq)]
// pub struct ScheduleRecordStep {
//     step: ScheduleStep,
//     options: HashSet<TaskId>
// }

// impl ScheduleRecordStep {
//     /// Create a record step from the chosen task and options
//     fn new(step: ScheduleStep, options_vec: &[TaskId]) -> Self {
//         let mut new_record = Self { step, options: HashSet::new() };

// 		for task_id in options_vec {
// 			new_record.options.insert(task_id.clone());
// 		}
		
// 		new_record
//     }
// }

/// A `DeterminismCheckScheduler` wraps an inner `Scheduler`, and when given a program,
/// decides whether the scheduling decisions can be deterministic or not.
/// Inner scheduler can be RoundRobin, Random, or PCT
#[derive(Debug)]
pub struct DeterminismCheckScheduler<S: ?Sized + Scheduler> {
    inner: Box<S>,
	iterations: usize,
	inner_iterations: usize,
	recording: bool,
	original_schedule: Vec<ScheduleRecord>,
	current_step: usize
}

impl<S: Scheduler> DeterminismCheckScheduler<S> {
    /// Create a new `DeterminismCheckScheduler` by wrapping the given `Scheduler` implementation.
    pub fn new(inner_iterations: usize,  inner: S) -> Self {
        Self {
            inner: Box::new(inner),
			iterations: 0,
			inner_iterations,
			original_schedule: Vec::new(),
			recording: false,
			current_step: 0
        }
    }

	/// Record an instance of non-determinism
	/// Should probably do something more informative here
	pub fn log_error(&mut self, message: &str) {
		assert!(false, "Found nondeterministic function! {}", message);
	}
}


impl<S: Scheduler> Scheduler for DeterminismCheckScheduler<S> {
    fn new_execution(&mut self) -> Option<Schedule> {

		if self.iterations % self.inner_iterations == 0 {
			// Start a new recording
			self.recording = true;
			self.original_schedule.clear();
		} else {
			if self.recording {
				self.recording = false;
			}

			if self.current_step != self.original_schedule.len() {
				panic!("Current execution ended earlier than original execution\n\tOriginal length: {}, acutal length: {}",
																self.original_schedule.len(), self.current_step);
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
			self.original_schedule.push(ScheduleRecord::new(ScheduleStep::Task(choice), runnable_tasks));

			self.current_step += 1;

			Some(choice)
				
		} else {
			// Determine whether the state is the same

			if self.current_step >= self.original_schedule.len() {
				// Current run is longer than original recording
				panic!("Current execution longer than expected. \n\tOriginal length: {}, acutal length: {}",
															self.original_schedule.len(), self.current_step);
			}

			let expected = self.original_schedule.get(self.current_step).unwrap().clone();
			let expected_step = expected.step;
			let expected_options = expected.runnable_tasks;
			let actual_options: HashSet<TaskId> = HashSet::from_iter(runnable_tasks.iter().cloned());

			match expected_step {
				ScheduleStep::Task(id) => { 
					if !expected_options.contains(&id) {
						panic!("\nTask chosen in recording is not currently runnable.\n\tExpected Task: {:?} \n\tActual options: {:?}\n",
																		id, actual_options);
					}

					if expected_options != actual_options {
						panic!("\nSet of runnable tasks is different than expected.\n\tExpected options: {:?} \n\tActual options: {:?}\n",
																		expected_options, actual_options);
					}

					self.current_step += 1;

					Some(id)
				},
				ScheduleStep::Random => { panic!("Found context switch, but recording expected random number generation")},
			}
		}
    }

    fn next_u64(&mut self) -> u64 {

		if self.recording {
			// Recording a schedule
			self.original_schedule.push(ScheduleRecord::new(ScheduleStep::Random, &Vec::new()));	
		} else {
			// Check that random step is recorded here
			if let ScheduleStep::Task(_) = self.original_schedule[self.current_step].step {
				panic!("Found random generation, but recording expected context switch");
			}
		}

		self.current_step += 1;
		self.inner.next_u64()

    }
}