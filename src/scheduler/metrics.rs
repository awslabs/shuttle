use crate::runtime::task::TaskId;
use crate::scheduler::Scheduler;
use tracing::info;

/// A `MetricsScheduler` wraps an inner `Scheduler` and collects metrics about the schedules it's
/// generating.
#[derive(Debug)]
pub(crate) struct MetricsScheduler<S> {
    inner: S,

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
}

impl<S: Scheduler> MetricsScheduler<S> {
    /// Create a new `MetricsScheduler` by wrapping the given `Scheduler` implementation.
    pub(crate) fn new(inner: S) -> Self {
        Self {
            inner,

            iterations: 0,
            iteration_divisor: 10,

            steps: 0,
            steps_metric: CountSummaryMetric::new(),

            last_task: TaskId::from(0),
            context_switches: 0,
            context_switches_metric: CountSummaryMetric::new(),
            preemptions: 0,
            preemptions_metric: CountSummaryMetric::new(),
        }
    }
}

impl<S: Scheduler> Scheduler for MetricsScheduler<S> {
    fn new_execution(&mut self) -> bool {
        if self.iterations > 0 {
            self.steps_metric.record(self.steps);
            self.steps = 0;

            self.context_switches_metric.record(self.context_switches);
            self.context_switches = 0;
            self.preemptions_metric.record(self.preemptions);
            self.preemptions = 0;

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

    fn next_task(&mut self, runnable_tasks: &[TaskId], current_task: Option<TaskId>) -> Option<TaskId> {
        let choice = self.inner.next_task(runnable_tasks, current_task)?;

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
}

impl<S> Drop for MetricsScheduler<S> {
    fn drop(&mut self) {
        info!(
            iterations = self.iterations - 1,
            steps = %self.steps_metric,
            context_switches = %self.context_switches_metric,
            preemptions = %self.preemptions,
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
