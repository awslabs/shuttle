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

    /// Which encoding to use when serializing a failing schedule. This only affects schedules
    /// Shuttle *writes*; both encodings can always be read back, so changing this does not
    /// invalidate schedules you have already saved.
    pub schedule_encoding: ScheduleEncoding,

    /// Which alphabet to use when rendering a serialized schedule as text. As with
    /// [`Config::schedule_encoding`], this only affects schedules Shuttle *writes*.
    pub schedule_text_encoding: ScheduleTextEncoding,
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
            schedule_encoding: ScheduleEncoding::default(),
            schedule_text_encoding: ScheduleTextEncoding::default(),
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

/// Specifies the alphabet used to render a serialized [`Schedule`](crate::scheduler::Schedule) as
/// text.
///
/// This is independent of [`ScheduleEncoding`], which chooses how the schedule's *bytes* are
/// produced; this chooses how those bytes are turned into a printable string. As with
/// `ScheduleEncoding`, deserialization detects the alphabet automatically, so changing this never
/// prevents an existing schedule from being replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScheduleTextEncoding {
    /// Choose between [`ScheduleTextEncoding::Hex`] and [`ScheduleTextEncoding::Unicode`] based on
    /// whether the destination can carry non-ASCII text. This is the default.
    ///
    /// A schedule written to a file always gets the Unicode alphabet, because a file is a byte sink.
    /// A schedule printed to a terminal gets it only if the locale says the terminal is expecting
    /// UTF-8, which is the mechanism POSIX defines for exactly this question: `LC_ALL`, then
    /// `LC_CTYPE`, then `LANG`, first one set wins. So a run under `LC_ALL=C`, or on a system whose
    /// locale is not configured, falls back to hex on its own.
    ///
    /// Note what this does *not* detect. Whether the terminal accepts UTF-8 says nothing about
    /// whether it renders stacked combining marks as zero-width, and no environment variable answers
    /// that. Measuring it would mean writing the text and then querying the cursor position, which
    /// needs raw mode on the same terminal, and Shuttle reports schedules from a panic hook, possibly
    /// while a panic is still unwinding. So `Auto` decides the alphabet, which is knowable, and
    /// leaves the stacking depth to you. If your terminal renders the marks as separate cells, set
    /// `Unicode { marks_per_cell: 0 }`, whose cost does not depend on the terminal at all.
    Auto,

    /// Render as hexadecimal.
    ///
    /// Four bits per character, and pure ASCII, so it survives anything. Use this if a schedule has
    /// to pass through tooling that mangles or strips non-ASCII text.
    Hex,

    /// Render using a dense Unicode alphabet, optionally stacking invisible combining marks to fit
    /// more data into each terminal column.
    ///
    /// Each column carries a 14-bit base character plus `marks_per_cell` combining marks of 8 bits
    /// each, against hex's 4 bits per column. At the default depth the whole schedule occupies a
    /// single cell and prints on one line, however long it is. A checksum is included, so a schedule
    /// damaged in transit is reported rather than silently replayed as a different schedule.
    ///
    /// Deeper stacking is denser but relies on the terminal treating every mark as zero-width.
    /// Terminals cap how many marks they will attach to one cell, and the cap varies between them.
    /// Past that cap a terminal either drops the excess, which damages the schedule and is what the
    /// checksum is there to catch, or renders each mark as its own cell, which is harmless but means
    /// the output takes one column per character rather than one per cell. If either bothers you,
    /// lower this, set it to zero to emit base characters only, or use [`ScheduleTextEncoding::Hex`]
    /// or [`FailurePersistence::File`] instead.
    Unicode {
        /// Number of combining marks to stack on each base character. Zero emits base characters
        /// only. Any value beyond the number of marks the payload needs simply puts the remainder of
        /// the schedule in the current cell, so a large value means "as few cells as possible".
        ///
        /// The depth is not recorded in the output. The decoder infers each character's width from
        /// the character itself, so schedules written at any depth are readable by any version.
        marks_per_cell: u32,
    },
}

impl ScheduleTextEncoding {
    /// Create a new default `ScheduleTextEncoding`.
    pub const fn new() -> Self {
        Self::Auto
    }

    /// What [`ScheduleTextEncoding::Auto`] resolves to when non-ASCII output is safe.
    ///
    /// More marks than any schedule has bits, so the whole payload lands in one cell: one column, and
    /// therefore one line, regardless of how long the schedule is.
    pub const DENSE: Self = Self::Unicode {
        marks_per_cell: u32::MAX,
    };

    /// Resolve [`ScheduleTextEncoding::Auto`] for a destination that either can or cannot carry
    /// non-ASCII text. Every other variant is returned unchanged, so this is idempotent and safe to
    /// apply more than once.
    pub const fn resolve(self, destination_accepts_non_ascii: bool) -> Self {
        match self {
            Self::Auto if destination_accepts_non_ascii => Self::DENSE,
            Self::Auto => Self::Hex,
            other => other,
        }
    }
}

/// Whether stderr, which is where Shuttle prints failing schedules, can carry non-ASCII text.
///
/// If stderr is not a terminal it is a file, a pipe or a captured test log, all of which are byte
/// sinks that take UTF-8 without complaint. If it is a terminal, the locale decides, per POSIX.
///
/// Note that libtest's output capture intercepts `eprintln!` above the file descriptor rather than by
/// replacing it, so this still sees the real terminal under `cargo test`, which is the terminal the
/// captured output is eventually replayed to.
pub fn stderr_accepts_non_ascii() -> bool {
    use std::io::IsTerminal;

    if !std::io::stderr().is_terminal() {
        return true;
    }
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|variable| std::env::var(variable).ok())
        .find(|value| !value.is_empty())
        .is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("utf-8") || value.contains("utf8")
        })
}

impl Default for ScheduleTextEncoding {
    fn default() -> Self {
        Self::new()
    }
}

/// Specifies how a [`Schedule`](crate::scheduler::Schedule) is encoded when it is serialized.
///
/// Deserialization always auto-detects the encoding from the schedule's leading magic byte, so this
/// setting only affects newly written schedules and never prevents an existing schedule from being
/// replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ScheduleEncoding {
    /// Encode each step as a fixed-width field, sized to hold the largest
    /// [`TaskId`](crate::runtime::task::TaskId) appearing anywhere in the schedule.
    ///
    /// This is simple and fast, but it pays for the largest task ID on *every* step, so it is a
    /// poor fit for long schedules over many tasks. Prefer [`ScheduleEncoding::MoveToFront`].
    FixedWidth,

    /// Encode each step as its rank in a move-to-front list of recently scheduled tasks.
    ///
    /// Schedules typically rotate among a small set of live tasks even when many tasks exist, so
    /// ranks are small and cluster near the front of the list. This is the default, and is
    /// substantially more compact than [`ScheduleEncoding::FixedWidth`] for long schedules.
    #[default]
    MoveToFront,
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
