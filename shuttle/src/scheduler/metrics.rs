use crate::runtime::task::{Task, TaskId};
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

    atomic_read: usize,
    atomic_read_metric: CountSummaryMetric,
    atomic_write: usize,
    atomic_write_metric: CountSummaryMetric,
    atomic_read_write: usize,
    atomic_read_write_metric: CountSummaryMetric,
    batch_semaphore_acq: usize,
    batch_semaphore_acq_metric: CountSummaryMetric,
    batch_semaphore_rel: usize,
    batch_semaphore_rel_metric: CountSummaryMetric,
    barrier_wait: usize,
    barrier_wait_metric: CountSummaryMetric,
    condvar_wait: usize,
    condvar_wait_metric: CountSummaryMetric,
    condvar_notify: usize,
    condvar_notify_metric: CountSummaryMetric,
    park: usize,
    park_metric: CountSummaryMetric,
    unpark: usize,
    unpark_metric: CountSummaryMetric,
    channel_send: usize,
    channel_send_metric: CountSummaryMetric,
    channel_recv: usize,
    channel_recv_metric: CountSummaryMetric,
    spawn: usize,
    spawn_metric: CountSummaryMetric,
    yield_event: usize,
    yield_event_metric: CountSummaryMetric,
    sleep: usize,
    sleep_metric: CountSummaryMetric,
    exit: usize,
    exit_metric: CountSummaryMetric,
    join: usize,
    join_metric: CountSummaryMetric,
    unknown: usize,
    unknown_metric: CountSummaryMetric,
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

            atomic_read: 0,
            atomic_read_metric: CountSummaryMetric::new(),
            atomic_write: 0,
            atomic_write_metric: CountSummaryMetric::new(),
            atomic_read_write: 0,
            atomic_read_write_metric: CountSummaryMetric::new(),
            batch_semaphore_acq: 0,
            batch_semaphore_acq_metric: CountSummaryMetric::new(),
            batch_semaphore_rel: 0,
            batch_semaphore_rel_metric: CountSummaryMetric::new(),
            barrier_wait: 0,
            barrier_wait_metric: CountSummaryMetric::new(),
            condvar_wait: 0,
            condvar_wait_metric: CountSummaryMetric::new(),
            condvar_notify: 0,
            condvar_notify_metric: CountSummaryMetric::new(),
            park: 0,
            park_metric: CountSummaryMetric::new(),
            unpark: 0,
            unpark_metric: CountSummaryMetric::new(),
            channel_send: 0,
            channel_send_metric: CountSummaryMetric::new(),
            channel_recv: 0,
            channel_recv_metric: CountSummaryMetric::new(),
            spawn: 0,
            spawn_metric: CountSummaryMetric::new(),
            yield_event: 0,
            yield_event_metric: CountSummaryMetric::new(),
            sleep: 0,
            sleep_metric: CountSummaryMetric::new(),
            exit: 0,
            exit_metric: CountSummaryMetric::new(),
            join: 0,
            join_metric: CountSummaryMetric::new(),
            unknown: 0,
            unknown_metric: CountSummaryMetric::new(),
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

        // Record and reset event counters
        self.atomic_read_metric.record(self.atomic_read);
        self.atomic_read = 0;
        self.atomic_write_metric.record(self.atomic_write);
        self.atomic_write = 0;
        self.atomic_read_write_metric.record(self.atomic_read_write);
        self.atomic_read_write = 0;
        self.batch_semaphore_acq_metric.record(self.batch_semaphore_acq);
        self.batch_semaphore_acq = 0;
        self.batch_semaphore_rel_metric.record(self.batch_semaphore_rel);
        self.batch_semaphore_rel = 0;
        self.barrier_wait_metric.record(self.barrier_wait);
        self.barrier_wait = 0;
        self.condvar_wait_metric.record(self.condvar_wait);
        self.condvar_wait = 0;
        self.condvar_notify_metric.record(self.condvar_notify);
        self.condvar_notify = 0;
        self.park_metric.record(self.park);
        self.park = 0;
        self.unpark_metric.record(self.unpark);
        self.unpark = 0;
        self.channel_send_metric.record(self.channel_send);
        self.channel_send = 0;
        self.channel_recv_metric.record(self.channel_recv);
        self.channel_recv = 0;
        self.spawn_metric.record(self.spawn);
        self.spawn = 0;
        self.yield_event_metric.record(self.yield_event);
        self.yield_event = 0;
        self.sleep_metric.record(self.sleep);
        self.sleep = 0;
        self.exit_metric.record(self.exit);
        self.exit = 0;
        self.join_metric.record(self.join);
        self.join = 0;
        self.unknown_metric.record(self.unknown);
        self.unknown = 0;
    }
}

impl<S: Scheduler> Scheduler for MetricsScheduler<S> {
    fn new_execution(&mut self) -> Option<Schedule> {
        if self.iterations > 0 {
            self.record_and_reset_metrics();

            if self.iterations.is_multiple_of(self.iteration_divisor) {
                info!(iterations = self.iterations);

                if self.iterations == self.iteration_divisor * 10 {
                    self.iteration_divisor *= 10;
                }
            }
        }
        self.iterations += 1;

        self.inner.new_execution()
    }

    fn next_task<'a>(
        &mut self,
        runnable_tasks: &'a [&'a Task],
        current_task: Option<TaskId>,
        is_yielding: bool,
    ) -> Option<&'a Task> {
        let chosen_task = self.inner.next_task(runnable_tasks, current_task, is_yielding)?;

        // Update event counters based on the chosen task's next_event
        use crate::runtime::task::Event;
        match chosen_task.next_event() {
            Event::AtomicRead(_, _) => self.atomic_read += 1,
            Event::AtomicWrite(_, _) => self.atomic_write += 1,
            Event::AtomicReadWrite(_, _) => self.atomic_read_write += 1,
            Event::BatchSemaphoreAcq(_, _) => self.batch_semaphore_acq += 1,
            Event::BatchSemaphoreRel(_, _) => self.batch_semaphore_rel += 1,
            Event::BarrierWait(_, _) => self.barrier_wait += 1,
            Event::CondvarWait(_, _) => self.condvar_wait += 1,
            Event::CondvarNotify(_) => self.condvar_notify += 1,
            Event::Park(_) => self.park += 1,
            Event::Unpark(_, _) => self.unpark += 1,
            Event::ChannelSend(_, _) => self.channel_send += 1,
            Event::ChannelRecv(_, _) => self.channel_recv += 1,
            Event::Spawn(_) => self.spawn += 1,
            Event::Yield(_) => self.yield_event += 1,
            Event::Sleep(_) => self.sleep += 1,
            Event::Exit => self.exit += 1,
            Event::Join(_, _) => self.join += 1,
            Event::Unknown => self.unknown += 1,
        }

        self.steps += 1;
        let choice_id = chosen_task.id();
        if choice_id != self.last_task {
            self.context_switches += 1;
            if runnable_tasks.iter().any(|t| t.id() == self.last_task) {
                self.preemptions += 1;
            }
        }
        self.last_task = choice_id;

        Some(chosen_task)
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
            atomic_read = %self.atomic_read_metric,
            atomic_write = %self.atomic_write_metric,
            atomic_read_write = %self.atomic_read_write_metric,
            batch_semaphore_acq = %self.batch_semaphore_acq_metric,
            batch_semaphore_rel = %self.batch_semaphore_rel_metric,
            barrier_wait = %self.barrier_wait_metric,
            condvar_wait = %self.condvar_wait_metric,
            condvar_notify = %self.condvar_notify_metric,
            park = %self.park_metric,
            unpark = %self.unpark_metric,
            channel_send = %self.channel_send_metric,
            channel_recv = %self.channel_recv_metric,
            spawn = %self.spawn_metric,
            yield_event = %self.yield_event_metric,
            sleep = %self.sleep_metric,
            exit = %self.exit_metric,
            join = %self.join_metric,
            unknown = %self.unknown_metric,
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
            write!(f, " avg={avg:.1}")?;
        }
        write!(f, "]")
    }
}
