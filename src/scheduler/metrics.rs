use crate::runtime::task::TaskId;
use crate::scheduler::{Schedule, Scheduler};
use tracing::info;

/// A `MetricsScheduler` wraps an inner `Scheduler` and collects metrics about the schedules it's
/// generating.
#[derive(Debug)]
pub(crate) struct MetricsScheduler<S: ?Sized + Scheduler> {
    inner: Box<S>,

    iterations: usize,
    iteration_divisor: usize,

    steps: usize,
    steps_metric: CountSummaryMetric,

    last_task: TaskId,
    // Number of times the scheduled task changed
    context_switches: usize,
    context_switches_metric: CountSummaryMetric,
    // Number of times the scheduled task changed when the previous task was still runnable
    preemptions: usize,
    preemptions_metric: CountSummaryMetric,

    random_choices: usize,
    random_choices_metric: CountSummaryMetric,
}

impl<S: Scheduler> MetricsScheduler<S> {
    /// Create a new `MetricsScheduler` by wrapping the given `Scheduler` implementation.
    pub(crate) fn new(inner: S) -> Self {
        Self {
            inner: Box::new(inner),

            iterations: 0,
            iteration_divisor: 10,

            steps: 0,
            steps_metric: CountSummaryMetric::new(),

            last_task: TaskId::from(0),
            context_switches: 0,
            context_switches_metric: CountSummaryMetric::new(),
            preemptions: 0,
            preemptions_metric: CountSummaryMetric::new(),

            random_choices: 0,
            random_choices_metric: CountSummaryMetric::new(),
        }
    }
}

impl<S: ?Sized + Scheduler> MetricsScheduler<S> {
    fn record_and_reset_metrics(&mut self) {
        self.steps_metric.record(self.steps);
        self.steps = 0;

        self.context_switches_metric.record(self.context_switches);
        self.context_switches = 0;
        self.preemptions_metric.record(self.preemptions);
        self.preemptions = 0;

        self.random_choices_metric.record(self.random_choices);
        self.random_choices = 0;
    }
}

impl<S: Scheduler> Scheduler for MetricsScheduler<S> {
    fn new_execution(&mut self) -> Option<Schedule> {
        if self.iterations > 0 {
            self.record_and_reset_metrics();

            if self.iterations % self.iteration_divisor == 0 {
                info!(iterations = self.iterations);

                if self.iterations == self.iteration_divisor * 10 {
                    self.iteration_divisor *= 10;
                }
            }
        }
        self.iterations += 1;

        self.inner.new_execution()
    }

    fn next_task(
        &mut self,
        runnable_tasks: &[TaskId],
        current_task: Option<TaskId>,
        is_yielding: bool,
    ) -> Option<TaskId> {
        let choice = self.inner.next_task(runnable_tasks, current_task, is_yielding)?;

        self.steps += 1;
        if choice != self.last_task {
            self.context_switches += 1;
            if runnable_tasks.contains(&self.last_task) {
                self.preemptions += 1;
            }
        }
        self.last_task = choice;

        Some(choice)
    }

    fn next_u64(&mut self) -> u64 {
        self.steps += 1;
        self.random_choices += 1;
        self.inner.next_u64()
    }
}

impl<S: ?Sized + Scheduler> Drop for MetricsScheduler<S> {
    fn drop(&mut self) {
        // If steps > 0 then we didn't get a chance to record the metrics for the current execution
        // (it's probably panicking), so record them now
        if self.steps > 0 {
            self.record_and_reset_metrics();
            self.iterations += 1;
        }

        info!(
            iterations = self.iterations.saturating_sub(1),
            steps = %self.steps_metric,
            context_switches = %self.context_switches_metric,
            preemptions = %self.preemptions_metric,
            random_choices = %self.random_choices_metric,
            "run finished"
        );
    }
}

/// A simple thing that can record a stream of `usize` values, and then report their min, max,
/// and average.
#[derive(Debug)]
struct CountSummaryMetric {
    min: usize,
    sum: usize,
    max: usize,
    n: usize,
}

impl CountSummaryMetric {
    fn new() -> Self {
        Self {
            min: usize::MAX,
            sum: 0,
            max: 0,
            n: 0,
        }
    }

    fn record(&mut self, value: usize) {
        if value < self.min {
            self.min = value;
        }
        if value > self.max {
            self.max = value;
        }
        self.sum += value;
        self.n += 1;
    }
}

impl std::fmt::Display for CountSummaryMetric {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "[min={}, max={},", self.min, self.max)?;
        if self.n > 0 {
            let avg = self.sum as f64 / self.n as f64;
            write!(f, " avg={:.1}", avg)?;
        }
        write!(f, "]")
    }
}
