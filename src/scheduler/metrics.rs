use crate::runtime::task::TaskId;
use crate::scheduler::serialization::serialize_schedule;
use crate::scheduler::{Schedule, Scheduler};
use std::cell::RefCell;
use std::rc::Rc;

/// A `MetricsScheduler` wraps an inner `Scheduler` and collects metrics about the schedules it's
/// generating. We use it mostly to remember the current schedule so that we can output it if a test
/// fails; that schedule can be used to replay the failure.
#[derive(Debug)]
pub(crate) struct MetricsScheduler {
    inner: Rc<RefCell<Box<dyn Scheduler>>>,
    num_iterations: usize,
    current_schedule: Schedule,
}

impl MetricsScheduler {
    /// Create a new `MetricsScheduler` by wrapping the given `Scheduler` implementation.
    pub(crate) fn new(inner: Rc<RefCell<Box<dyn Scheduler>>>) -> Self {
        Self {
            inner,
            num_iterations: 0,
            current_schedule: Schedule::new(),
        }
    }

    /// Return the schedule so far for the current iteration.
    pub(crate) fn current_schedule(&self) -> &Schedule {
        &self.current_schedule
    }

    /// Return the schedule so far for the current iteration, but serialized into a form that can be
    /// easily printed out and read back in for replay purposes.
    pub(crate) fn serialized_schedule(&self) -> String {
        serialize_schedule(self.current_schedule())
    }
}

impl Scheduler for MetricsScheduler {
    fn new_execution(&mut self) -> bool {
        self.num_iterations += 1;
        self.current_schedule.clear();
        self.inner.borrow_mut().new_execution()
    }

    fn next_task(&mut self, runnable_tasks: &[TaskId], current_task: Option<TaskId>) -> Option<TaskId> {
        let choice = self.inner.borrow_mut().next_task(runnable_tasks, current_task)?;
        self.current_schedule.push(choice);
        Some(choice)
    }
}
