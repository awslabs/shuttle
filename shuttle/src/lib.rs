#![deny(warnings, missing_debug_implementations, missing_docs)]

//! Shuttle is a library for testing concurrent Rust code, heavily inspired by [Loom][].
//!
//! Shuttle focuses on randomized testing, rather than the exhaustive testing that Loom offers. This
//! is a soundness—scalability trade-off: Shuttle is not sound (a passing Shuttle test does not
//! prove the code is correct), but it scales to much larger test cases than Loom. Empirically,
//! randomized testing is successful at finding most concurrency bugs, which tend not to be
//! adversarial.
//!
//! ## Testing concurrent code
//!
//! Consider this simple piece of concurrent code:
//!
//! ```no_run
//! use std::sync::{Arc, Mutex};
//! use std::thread;
//!
//! let lock = Arc::new(Mutex::new(0u64));
//! let lock2 = lock.clone();
//!
//! thread::spawn(move || {
//!     *lock.lock().unwrap() = 1;
//! });
//!
//! assert_eq!(0, *lock2.lock().unwrap());
//! ```
//!
//! There is an obvious race condition here: if the spawned thread runs before the assertion, the
//! assertion will fail. But writing a unit test that finds this execution is tricky. We could run
//! the test many times and try to "get lucky" by finding a failing execution, but that's not a very
//! reliable testing approach. Even if the test does fail, it will be difficult to debug: we won't
//! be able to easily catch the failure in a debugger, and every time we make a change, we will need
//! to run the test many times to decide whether we fixed the issue.
//!
//! ### Randomly testing concurrent code with Shuttle
//!
//! Shuttle avoids this issue by controlling the scheduling of each thread in the program, and
//! scheduling those threads *randomly*. By controlling the scheduling, Shuttle allows us to
//! reproduce failing tests deterministically. By using random scheduling, with appropriate
//! heuristics, Shuttle can still catch most (non-adversarial) concurrency bugs even though it is
//! not an exhaustive checker.
//!
//! A Shuttle version of the above test just wraps the test body in a call to Shuttle's
//! [check_random] function, and replaces the concurrency-related imports from `std` with imports
//! from `shuttle`:
//!
//! ```should_panic
//! use shuttle::sync::{Arc, Mutex};
//! use shuttle::thread;
//!
//! shuttle::check_random(|| {
//!     let lock = Arc::new(Mutex::new(0u64));
//!     let lock2 = lock.clone();
//!
//!     thread::spawn(move || {
//!         *lock.lock().unwrap() = 1;
//!     });
//!
//!     assert_eq!(0, *lock2.lock().unwrap());
//! }, 100);
//! ```
//!
//! This test detects the assertion failure with extremely high probability (over 99.9999%).
//!
//! ## Testing non-deterministic code
//!
//! Shuttle supports testing code that uses *data non-determinism* (random number generation). For
//! example, this test uses the [`rand`](https://crates.io/crates/rand) crate to generate a random
//! number:
//!
//! ```no_run
//! use rand::{thread_rng, Rng};
//!
//! let x = thread_rng().gen::<u64>();
//! assert_eq!(x % 10, 7);
//! ```
//!
//! Shuttle provides its own implementation of [`rand`] that is a drop-in replacement:
//!
//! ```should_panic
//! use shuttle::rand::{thread_rng, Rng};
//!
//! shuttle::check_random(|| {
//!     let x = thread_rng().gen::<u64>();
//!     assert_ne!(x % 10, 7);
//! }, 100);
//! ```
//!
//! This test will run the body 100 times, and fail if any of those executions fails; the test
//! therefore fails with probability 1-(9/10)^100, or 99.997%. We can increase the `100` parameter
//! to run more executions and increase the probability of finding the failure. Note that Shuttle
//! isn't doing anything special to increase the probability of this test failing other than running
//! the body multiple times.
//!
//! When this test fails, Shuttle provides output that can be used to **deterministically**
//! reproduce the failure:
//!
//! ```text
//! test panicked in task "task-0" with schedule: "910102ccdedf9592aba2afd70104"
//! pass that schedule string into `shuttle::replay` to reproduce the failure
//! ```
//!
//! We can use Shuttle's [`replay`] function to replay the execution that causes the failure:
//!
//! ```should_panic
//! # // *** DON'T FORGET TO UPDATE THE TEXT OUTPUT RIGHT ABOVE THIS IF YOU CHANGE THIS TEST! ***
//! use shuttle::rand::{thread_rng, Rng};
//!
//! shuttle::replay(|| {
//!     let x = thread_rng().gen::<u64>();
//!     assert_ne!(x % 10, 7);
//! }, "910102ccdedf9592aba2afd70104");
//! ```
//!
//! This runs the test only once, and is guaranteed to reproduce the failure.
//!
//! Support for data non-determinism is most useful when *combined* with support for schedule
//! non-determinism (i.e., concurrency). For example, an integration test might spawn several
//! threads, and within each thread perform a random sequence of actions determined by `thread_rng`
//! (this style of testing is often referred to as a "stress test"). By using Shuttle to implement
//! the stress test, we can both increase the coverage of the test by exploring more thread
//! interleavings and allow test failures to be deterministically reproducible for debugging.
//!
//! ## Writing Shuttle tests
//!
//! To test concurrent code with Shuttle, all uses of synchronization primitives from `std` must be
//! replaced by their Shuttle equivalents. The simplest way to do this is via `cfg` flags.
//! Specifically, if you enforce that all synchronization primitives are imported from a single
//! `sync` module in your code, and implement that module like this:
//!
//! ```
//! #[cfg(all(feature = "shuttle", test))]
//! use shuttle::{sync::*, thread};
//! #[cfg(not(all(feature = "shuttle", test)))]
//! use std::{sync::*, thread};
//! ```
//!
//! Then a Shuttle test can be written like this:
//!
//! ```
//! # mod my_crate {}
//! #[cfg(feature = "shuttle")]
//! #[test]
//! fn concurrency_test_shuttle() {
//!     use my_crate::*;
//!     // ...
//! }
//! ```
//!
//! and be executed by running `cargo test --features shuttle`.
//!
//! ### Choosing a scheduler and running a test
//!
//! Shuttle tests need to choose a *scheduler* to use to direct the execution. The scheduler
//! determines the order in which threads are scheduled. Different scheduling policies can increase
//! the probability of detecting certain classes of bugs (e.g., race conditions), but at the cost of
//! needing to test more executions.
//!
//! Shuttle has a number of built-in schedulers, which implement the
//! [`Scheduler`](crate::scheduler::Scheduler) trait. They are most easily accessed via convenience
//! methods:
//! - [`check_random`] runs a test using a random scheduler for a chosen number of executions.
//! - [`check_pct`] runs a test using the [Probabilistic Concurrency Testing][pct] (PCT) algorithm.
//!   PCT bounds the number of preemptions a test explores; empirically, most concurrency bugs can
//!   be detected with very few preemptions, and so PCT increases the probability of finding such
//!   bugs. The PCT scheduler can be configured with a "bug depth" (the number of preemptions) and a
//!   number of executions.
//! - [`check_dfs`] runs a test with an *exhaustive* scheduler using depth-first search. Exhaustive
//!   testing is intractable for all but the very simplest programs, and so using this scheduler is
//!   not recommended, but it can be useful to thoroughly test small concurrency primitives. The DFS
//!   scheduler can be configured with a bound on the depth of schedules to explore.
//!
//! When these convenience methods do not provide enough control, Shuttle provides a [`Runner`]
//! object for executing a test. A runner is constructed from a chosen [scheduler], and
//! then invoked with the [`Runner::run`] method. Shuttle also provides a [`PortfolioRunner`] object
//! for running multiple schedulers, using parallelism to increase the number of test executions
//! explored.
//!
//! [Loom]: https://github.com/tokio-rs/loom
//! [pct]: https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/asplos277-pct.pdf

pub mod annotations;
pub mod future;
pub mod hint;
pub mod lazy_static;
pub mod rand;
pub mod sync;
pub mod thread;

pub mod current;
pub mod scheduler;

mod runtime;

pub use runtime::runner::{PortfolioRunner, Runner};

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

    /// Time limit for an entire test. If set, calls to [`Runner::run`] will return when the time
    /// limit is exceeded or the [`Scheduler`](crate::scheduler::Scheduler) chooses to stop (e.g.,
    /// by hitting its maximum number of iterations), whichever comes first. This time limit will
    /// not abort a currently running test iteration; the limit is only checked between iterations.
    pub max_time: Option<std::time::Duration>,

    /// Whether to silence warnings about Shuttle behaviors that may miss bugs or introduce false
    /// positives:
    /// 1. [Unsound implementation of `atomic`](crate::sync::atomic#warning-about-relaxed-behaviors)
    ///    may miss bugs
    /// 2. [`lazy_static` values are dropped](mod@crate::lazy_static) at the end of an execution
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

    // EXPERIMENTAL; may be removed in the future. May also be changed to an enum to force biased scheduling.
    /// By default (when this is `false`) when a task panics we will serialize the schedule, then
    /// continue scheduling until the panicking task has fully unwound its stack, and only then return.
    /// This is somewhat wasteful, and also exposes us to more chances of having the entire test abort,
    /// as we are running test code with `std::thread::panicking` (thus a second panic will be an abort).
    /// Setting this to `true` will cause scheduling to stop as soon as a task panics. Note that the chance of
    /// an abort (after serializing the schedule) is still present, as we will resume the unwind, and may panic
    /// while calling drop handlers.
    pub immediately_return_on_panic: bool,
}

impl Config {
    /// Create a new default configuration
    pub fn new() -> Self {
        Self {
            stack_size: 0x8000,
            failure_persistence: FailurePersistence::Print,
            max_steps: MaxSteps::FailAfter(1_000_000),
            max_time: None,
            silence_warnings: false,
            record_steps_in_span: false,
            immediately_return_on_panic: false,
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
/// By default, schedules are printed to stdout/stderr, and can be replayed using [`replay`].
/// Optionally, they can instead be persisted to a file and replayed using [`replay_from_file`],
/// which can be useful if the schedule is too large to conveniently include in a call to
/// [`replay`].
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
/// (non-concurrency-operation) code between acquiring and releasing a [`Mutex`] is a single step.
/// Shuttle can bound the maximum number of steps a single test iteration can take to prevent
/// infinite loops. If the bound is hit, the test can either fail (`FailAfter`) or continue to the
/// next iteration (`ContinueAfter`).
///
/// The steps bound can be used to protect against livelock and fairness issues. For example, if a
/// thread is waiting for another thread to make progress, but the chosen [`Scheduler`] never
/// schedules that thread, a livelock occurs and the test will not terminate without a step bound.
///
/// By default, Shuttle fails a test after 1,000,000 steps.
///
/// [`Mutex`]: crate::sync::Mutex
/// [`Scheduler`]: crate::scheduler::Scheduler
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

/// Run the given function once under a round-robin concurrency scheduler.
// TODO consider removing this -- round robin scheduling is never what you want.
#[doc(hidden)]
pub fn check<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::RoundRobinScheduler;

    let runner = Runner::new(RoundRobinScheduler::new(1), Default::default());
    runner.run(f);
}

/// Run the given function under a *uniformly* random scheduler for some number of iterations.
/// Each iteration will run a (potentially) different randomized schedule.
pub fn check_urw<F>(f: F, iterations: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::UrwRandomScheduler;

    let runner = Runner::new(UrwRandomScheduler::new(iterations), Default::default());
    runner.run(f);
}

/// Run the given function under a randomized concurrency scheduler for some number of iterations.
/// Each iteration will run a (potentially) different randomized schedule.
pub fn check_random<F>(f: F, iterations: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::RandomScheduler;

    let runner = Runner::new(RandomScheduler::new(iterations), Default::default());
    runner.run(f);
}

/// Run the given function under the randomized POS scheduler for some number of iterations.
/// Each iteration will run a (potentially) different randomized schedule.
pub fn check_pos<F>(f: F, iterations: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::PosScheduler;

    let runner = Runner::new(PosScheduler::new(iterations), Default::default());
    runner.run(f);
}

/// Run function `f` using `RandomScheduler` initialized with the provided `seed` for the given
/// `iterations`.
/// This makes generating the random seed for each execution independent from `RandomScheduler`.
/// Therefore, this can be used with a library (like proptest) that takes care of generating the
/// random seeds.
pub fn check_random_with_seed<F>(f: F, seed: u64, iterations: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::RandomScheduler;

    let scheduler = RandomScheduler::new_from_seed(seed, iterations);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(f);
}

/// Run the given function under a PCT concurrency scheduler for some number of iterations at the
/// given depth. Each iteration will run a (potentially) different randomized schedule.
pub fn check_pct<F>(f: F, iterations: usize, depth: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::PctScheduler;

    let scheduler = PctScheduler::new(depth, iterations);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(f);
}

/// Run the given function under a depth-first-search scheduler until all interleavings have been
/// explored (but if the max_iterations bound is provided, stop after that many iterations).
pub fn check_dfs<F>(f: F, max_iterations: Option<usize>)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::DfsScheduler;

    let scheduler = DfsScheduler::new(max_iterations, false);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(f);
}

/// Run the given function under a scheduler that checks whether the function
/// contains randomness which is not controlled by Shuttle.
/// Each iteration will check a different random schedule and replay that schedule once.
pub fn check_uncontrolled_nondeterminism<F>(f: F, max_iterations: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::RandomScheduler;
    use crate::scheduler::UncontrolledNondeterminismCheckScheduler;

    let scheduler = UncontrolledNondeterminismCheckScheduler::new(RandomScheduler::new(max_iterations));

    let runner = Runner::new(scheduler, Config::default());
    runner.run(f);
}

/// Run the given function according to a given encoded schedule, usually produced as the output of
/// a failing Shuttle test case.
///
/// This function allows deterministic replay of a failing schedule, as long as `f` contains no
/// non-determinism other than that introduced by scheduling.
///
/// This is a convenience function for constructing a [`Runner`] that uses
/// [`ReplayScheduler::new_from_encoded`](scheduler::ReplayScheduler::new_from_encoded).
pub fn replay<F>(f: F, encoded_schedule: &str)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::ReplayScheduler;

    let scheduler = ReplayScheduler::new_from_encoded(encoded_schedule);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(f);
}

/// Run the given function according to a given encoded schedule, usually produced as the output of
/// a failing Shuttle test case, while recording an annotated schedule, for use with the Shuttle
/// Explorer extension.
///
/// This function allows deterministic replay of a failing schedule, as long as `f` contains no
/// non-determinism other than that introduced by scheduling.
///
/// This is a convenience function for constructing a [`Runner`] that uses
/// an [`crate::scheduler::AnnotationScheduler`] wrapping a replay scheduler created with
/// [`ReplayScheduler::new_from_encoded`](scheduler::ReplayScheduler::new_from_encoded).
pub fn annotate_replay<F>(f: F, encoded_schedule: &str)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::{AnnotationScheduler, ReplayScheduler};

    let scheduler_inner = ReplayScheduler::new_from_encoded(encoded_schedule);
    let scheduler = AnnotationScheduler::new(scheduler_inner);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(f);
}

/// Run the given function according to a schedule saved in the given file, usually produced as the
/// output of a failing Shuttle test case.
///
/// This function allows deterministic replay of a failing schedule, as long as `f` contains no
/// non-determinism other than that introduced by scheduling.
///
/// This is a convenience function for constructing a [`Runner`] that uses
/// [`ReplayScheduler::new_from_file`](scheduler::ReplayScheduler::new_from_file).
pub fn replay_from_file<F, P>(f: F, path: P)
where
    F: Fn() + Send + Sync + 'static,
    P: AsRef<std::path::Path>,
{
    use crate::scheduler::ReplayScheduler;

    let scheduler = ReplayScheduler::new_from_file(path).expect("could not load schedule from file");
    let runner = Runner::new(scheduler, Default::default());
    runner.run(f);
}

/// Declare a new thread local storage key of type [`LocalKey`](crate::thread::LocalKey).
#[macro_export]
macro_rules! thread_local {
    // empty (base case for the recursion)
    () => {};

    // process multiple declarations with a const initializer
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = const { $init:expr }; $($rest:tt)*) => (
        $crate::__thread_local_inner!($(#[$attr])* $vis $name, $t, $init);
        $crate::thread_local!($($rest)*);
    );

    // handle a single declaration with a const initializer
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = const { $init:expr }) => (
        $crate::__thread_local_inner!($(#[$attr])* $vis $name, $t, $init);
    );

    // process multiple declarations
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = $init:expr; $($rest:tt)*) => (
        $crate::__thread_local_inner!($(#[$attr])* $vis $name, $t, $init);
        $crate::thread_local!($($rest)*);
    );

    // handle a single declaration
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = $init:expr) => (
        $crate::__thread_local_inner!($(#[$attr])* $vis $name, $t, $init);
    );
}

#[doc(hidden)]
#[macro_export]
macro_rules! __thread_local_inner {
    ($(#[$attr:meta])* $vis:vis $name:ident, $t:ty, $init:expr) => {
        $(#[$attr])* $vis static $name: $crate::thread::LocalKey<$t> =
            $crate::thread::LocalKey {
                init: || { $init },
                _p: std::marker::PhantomData,
            };
    }
}

/// Declare a new [lazy static value](crate::lazy_static::Lazy), like the `lazy_static` crate.
// These macros are copied from the lazy_static crate.
#[macro_export]
macro_rules! lazy_static {
    ($(#[$attr:meta])* static ref $N:ident : $T:ty = $e:expr; $($t:tt)*) => {
        // use `()` to explicitly forward the information about private items
        $crate::__lazy_static_internal!($(#[$attr])* () static ref $N : $T = $e; $($t)*);
    };
    ($(#[$attr:meta])* pub static ref $N:ident : $T:ty = $e:expr; $($t:tt)*) => {
        $crate::__lazy_static_internal!($(#[$attr])* (pub) static ref $N : $T = $e; $($t)*);
    };
    ($(#[$attr:meta])* pub ($($vis:tt)+) static ref $N:ident : $T:ty = $e:expr; $($t:tt)*) => {
        $crate::__lazy_static_internal!($(#[$attr])* (pub ($($vis)+)) static ref $N : $T = $e; $($t)*);
    };
    () => ()
}

#[macro_export]
#[doc(hidden)]
macro_rules! __lazy_static_internal {
    // optional visibility restrictions are wrapped in `()` to allow for
    // explicitly passing otherwise implicit information about private items
    ($(#[$attr:meta])* ($($vis:tt)*) static ref $N:ident : $T:ty = $e:expr; $($t:tt)*) => {
        $crate::__lazy_static_internal!(@MAKE TY, $(#[$attr])*, ($($vis)*), $N);
        $crate::__lazy_static_internal!(@TAIL, $N : $T = $e);
        $crate::lazy_static!($($t)*);
    };
    (@TAIL, $N:ident : $T:ty = $e:expr) => {
        impl ::std::ops::Deref for $N {
            type Target = $T;
            fn deref(&self) -> &$T {
                #[inline(always)]
                fn __static_ref_initialize() -> $T { $e }

                #[inline(always)]
                fn __stability() -> &'static $T {
                    static LAZY: $crate::lazy_static::Lazy<$T> = $crate::lazy_static::Lazy::new(__static_ref_initialize);
                    LAZY.get()
                }
                __stability()
            }
        }
        impl $crate::lazy_static::LazyStatic for $N {
            fn initialize(lazy: &Self) {
                let _ = &**lazy;
            }
        }
    };
    // `vis` is wrapped in `()` to prevent parsing ambiguity
    (@MAKE TY, $(#[$attr:meta])*, ($($vis:tt)*), $N:ident) => {
        #[allow(missing_copy_implementations)]
        #[allow(non_camel_case_types)]
        #[allow(dead_code)]
        $(#[$attr])*
        $($vis)* struct $N {__private_field: ()}
        #[doc(hidden)]
        $($vis)* static $N: $N = $N {__private_field: ()};
    };
    () => ()
}
