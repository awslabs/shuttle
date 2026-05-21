use std::cell::Cell;

/// Configuration parameters for Shuttle
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
    /// Stack size allocated for each thread
    pub stack_size: usize,

    /// How to persist schedules when a test fails
    pub failure_persistence: FailurePersistence,

    /// Maximum number of steps a single iteration of a test can take, and how to react when the
    /// limit is reached
    pub max_steps: MaxSteps,

    /// Time limit for an entire test. If set, calls to [`crate::runtime::runner::Runner::run`] will return when the time
    /// limit is exceeded or the [`Scheduler`](crate::scheduler::Scheduler) chooses to stop (e.g.,
    /// by hitting its maximum number of iterations), whichever comes first. This time limit will
    /// not abort a currently running test iteration; the limit is only checked between iterations.
    pub max_time: Option<std::time::Duration>,

    /// Whether to silence warnings about Shuttle behaviors that may miss bugs or introduce false
    /// positives:
    /// 1. Unsound implementation of `atomic` may miss bugs
    /// 2. `lazy_static` values are dropped at the end of an execution
    pub silence_warnings: bool,

    /// Whether to call the `Span::record()` method to update the step count (`i`) of the `Span`
    /// containing the `TaskId` and the current step count for the given `TaskId`.
    /// If `false`, this `Span` will look like this: `step{task=1}`, and if `true`, this `Span`
    /// will look something like this: `step{task=1 i=3 i=9 i=12}`, or, if a `Subscriber` which
    /// overwrites on calls to `span.record()` is used, something like this:
    /// ```text
    /// step{task=1 i=3}
    /// step{task=1 i=9}
    /// step{task=1 i=12}
    /// ```
    /// The reason this is a config option is that the most popular tracing `Subscriber`s, ie
    /// `tracing_subscriber::fmt`, appends to the span on calls to `record()` (instead of
    /// overwriting), which results in traces which are hard to read if the task is scheduled more
    /// than a few times.
    /// Thus: set `record_steps_in_span` to `true` if you want "append behavior", or if you are using
    /// a `Subscriber` which overwrites on calls to `record()` and want to display the current step
    /// count.
    pub record_steps_in_span: bool,

    /// The config to define how to handle ungraceful shutdowns, ie. when the test panics.
    pub ungraceful_shutdown_config: UngracefulShutdownConfig,
}

std::thread_local! {
    pub static UNGRACEFUL_SHUTDOWN_CONFIG: Cell<UngracefulShutdownConfig> = const { Cell::new(UngracefulShutdownConfig::new()) };
}

#[derive(Copy, Clone, Debug)]
#[non_exhaustive]
/// What to do with the continuation function when a task panics.
/// Modelled as a non-exhaustive enum because there are a couple of unimplemented behaviors, such as
/// returning the continuation function, or sending the function to a "sacrificial" thread to be dropped
pub enum ContinuationFunctionBehavior {
    /// Drop the continuation function when a task panics.
    Drop,
    /// Leak the continuation function when a task panics.
    Leak,
}

impl ContinuationFunctionBehavior {
    /// Create a new default `ContinuationFunctionBehavior`
    pub const fn new() -> Self {
        // This is the default because most Shuttle tests are not written in a "collect" mode, meaning
        // the volume of leaks is low, and because we already default to leaking the continuation itself (via
        // `force_reset`), which is a much bigger memory leak.
        Self::Leak
    }
}

impl Default for ContinuationFunctionBehavior {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Copy, Clone, Debug)]
#[non_exhaustive]
/// The config to define how to handle ungraceful shutdowns, ie. when the test panics.
pub struct UngracefulShutdownConfig {
    /// By default (when this is `false`) when a task panics we will serialize the schedule, then
    /// continue scheduling until the panicking task has fully unwound its stack, and only then return.
    /// This is somewhat wasteful, and also exposes us to more chances of having the entire test abort,
    /// as we are running test code with `std::thread::panicking` (thus a second panic will be an abort).
    /// Setting this to `true` will cause scheduling to stop as soon as a task panics. Note that the chance of
    /// an abort (after serializing the schedule) is still present, as we will resume the unwind, and may panic
    /// while calling drop handlers.
    pub immediately_return_on_panic: bool,

    /// What to do with the continuation function when it is dropped after a panic.
    pub continuation_function_behavior: ContinuationFunctionBehavior,
}

impl UngracefulShutdownConfig {
    /// Create a new default `UngracefulShutdownConfig`
    pub const fn new() -> Self {
        Self {
            immediately_return_on_panic: false,
            continuation_function_behavior: ContinuationFunctionBehavior::new(),
        }
    }
}

impl Default for UngracefulShutdownConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl Config {
    /// Create a new default configuration
    pub fn new() -> Self {
        Self {
            stack_size: 0xf000,
            failure_persistence: FailurePersistence::Print,
            max_steps: MaxSteps::FailAfter(1_000_000),
            max_time: None,
            silence_warnings: false,
            record_steps_in_span: false,
            ungraceful_shutdown_config: UngracefulShutdownConfig::default(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

/// Specifies how to persist schedules when a Shuttle test fails
///
/// By default, schedules are printed to stdout/stderr, and can be replayed using `replay`.
/// Optionally, they can instead be persisted to a file and replayed using `replay_from_file`,
/// which can be useful if the schedule is too large to conveniently include in a call to
/// `replay`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailurePersistence {
    /// Do not persist failing schedules
    None,
    /// Print failing schedules to stdout/stderr
    Print,
    /// Persist schedules as files in the given directory, or the current directory if None.
    File(Option<std::path::PathBuf>),
}

/// Specifies an upper bound on the number of steps a single iteration of a Shuttle test can take,
/// and how to react when the bound is reached.
///
/// A "step" is an atomic region (all the code between two yieldpoints). For example, all the
/// (non-concurrency-operation) code between acquiring and releasing a `Mutex` is a single step.
/// Shuttle can bound the maximum number of steps a single test iteration can take to prevent
/// infinite loops. If the bound is hit, the test can either fail (`FailAfter`) or continue to the
/// next iteration (`ContinueAfter`).
///
/// The steps bound can be used to protect against livelock and fairness issues. For example, if a
/// thread is waiting for another thread to make progress, but the chosen `Scheduler` never
/// schedules that thread, a livelock occurs and the test will not terminate without a step bound.
///
/// By default, Shuttle fails a test after 1,000,000 steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaxSteps {
    /// Do not enforce any bound on the maximum number of steps
    None,
    /// Fail the test (by panicking) after the given number of steps
    FailAfter(usize),
    /// When the given number of steps is reached, stop the current iteration of the test and
    /// begin a new iteration
    ContinueAfter(usize),
}
