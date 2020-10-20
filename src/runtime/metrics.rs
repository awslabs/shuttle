use crate::runtime::task::serialization::serialize_schedule;
use crate::runtime::task::TaskId;
use crate::scheduler::Scheduler;

/// A `MetricsScheduler` wraps an inner `Scheduler` and collects metrics about the schedules it's
/// generating. We use it mostly to remember the current schedule so that we can output it if a test
/// fails; that schedule can be used to replay the failure.
#[derive(Debug)]
pub(crate) struct MetricsScheduler<'a> {
    inner: &'a mut Box<dyn Scheduler>,
    num_iterations: usize,
    current_schedule: Vec<TaskId>,
}

impl<'a> MetricsScheduler<'a> {
    /// Create a new `MetricsScheduler` by wrapping the given `Scheduler` implementation.
    pub(crate) fn new(inner: &'a mut Box<dyn Scheduler>) -> Self {
        Self {
            inner,
            num_iterations: 0,
            current_schedule: vec![],
        }
    }

    /// Return the schedule so far for the current iteration.
    pub(crate) fn current_schedule(&self) -> &Vec<TaskId> {
        &self.current_schedule
    }

    /// Return the schedule so far for the current iteration, but serialized into a form that can be
    /// easily printed out and read back in for replay purposes.
    pub(crate) fn serialized_schedule(&self) -> String {
        serialize_schedule(self.current_schedule())
    }
}

impl Scheduler for MetricsScheduler<'_> {
    fn new_execution(&mut self) -> bool {
        self.num_iterations += 1;
        self.current_schedule.clear();
        self.inner.new_execution()
    }

    fn next_task(&mut self, runnable_tasks: &[TaskId], current_task: Option<TaskId>) -> TaskId {
        let choice = self.inner.next_task(runnable_tasks, current_task);
        self.current_schedule.push(choice);
        choice
    }
}
