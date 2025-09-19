use crate::annotations::{record_random, record_schedule, start_annotations, stop_annotations};
use crate::runtime::task::{Task, TaskId};
use crate::scheduler::{Schedule, Scheduler};

/// An `AnnotationScheduler` wraps an inner `Scheduler` and enables the
/// creation of an annotated schedule (for use with Shuttle Explorer).
/// Without enabling the feature `annotation`, this wrapper does nothing.
///
/// Importantly, only one `AnnotationScheduler` instance should exist at a time.
#[derive(Debug)]
pub struct AnnotationScheduler<S: ?Sized + Scheduler>(Box<S>);

impl<S: Scheduler> AnnotationScheduler<S> {
    /// Create a new `AnnotationScheduler` by wrapping the given `Scheduler` implementation.
    pub fn new(inner: S) -> Self {
        start_annotations();
        Self(Box::new(inner))
    }
}

impl<S: Scheduler> Scheduler for AnnotationScheduler<S> {
    fn new_execution(&mut self) -> Option<Schedule> {
        self.0.new_execution()
    }

    fn next_task<'a>(
        &mut self,
        runnable_tasks: &'a [&'a Task],
        current_task: Option<TaskId>,
        is_yielding: bool,
    ) -> Option<&'a Task> {
        let choice = self.0.next_task(runnable_tasks, current_task, is_yielding)?;
        record_schedule(choice.id(), runnable_tasks);
        Some(choice)
    }

    fn next_u64(&mut self) -> u64 {
        record_random();
        self.0.next_u64()
    }
}

impl<S: ?Sized + Scheduler> Drop for AnnotationScheduler<S> {
    fn drop(&mut self) {
        stop_annotations();
    }
}
