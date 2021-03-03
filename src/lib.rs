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
//! object for executing a test. A runner is constructed from a chosen scheduler, and then invoked
//! with the [`Runner::run`] method. Shuttle also provides a [`PortfolioRunner`] object for running
//! multiple schedulers, using parallelism to increase the number of test executions explored.
//!
//! [Loom]: https://github.com/tokio-rs/loom
//! [pct]: https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/asplos277-pct.pdf

pub mod asynch;
pub mod rand;
pub mod sync;
pub mod thread;

pub mod scheduler;

mod runtime;

pub use runtime::runner::{PortfolioRunner, Runner};

/// Configuration parameters for Shuttle
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Config {
    /// Maximum number of supported tasks (includes threads and async tasks)
    pub max_tasks: usize,

    /// Stack size allocated for each thread
    pub stack_size: usize,
}

impl Config {
    /// Create a new default configuration
    pub fn new() -> Self {
        Self {
            max_tasks: 16usize,
            stack_size: 0x8000,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

/// Run the given function once under a round-robin concurrency scheduler.
// TODO consider removing this -- round robin scheduling is never what you want.
#[doc(hidden)]
pub fn check<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::RoundRobinScheduler;

    let runner = Runner::new(RoundRobinScheduler::new(), Default::default());
    runner.run(f);
}

/// Run the given function under a randomized concurrency scheduler for some number of iterations.
/// Each iteration will run a (potentially) different randomized schedule.
pub fn check_random<F>(f: F, iterations: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::RandomScheduler;

    let scheduler = RandomScheduler::new(iterations);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(f);
}

/// Run the given function under a PCT concurrency scheduler for some number of iterations at the
/// given depth. Each iteration will run a (potentially) different randomized schedule.
pub fn check_pct<F>(f: F, iterations: usize, depth: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::PCTScheduler;

    let scheduler = PCTScheduler::new(depth, iterations);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(f);
}

/// Run the given function under a depth-first-search scheduler until all interleavings have been
/// explored (but if the max_iterations bound is provided, stop after that many iterations;
/// and if the max_depth bound is provided, stop exploring any execution when the depth is reached).
pub fn check_dfs<F>(f: F, max_iterations: Option<usize>, max_depth: Option<usize>)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::DFSScheduler;

    let scheduler = DFSScheduler::new(max_iterations, max_depth, false);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(f);
}

/// Run the given function according to a given encoded schedule, usually produced as the output
/// of a failing Shuttle test case.
///
/// This function allows deterministic replay of a failing schedule, as long as `f` contains no
/// non-determinism other than that introduced by scheduling.
pub fn replay<F>(f: F, encoded_schedule: &str)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::ReplayScheduler;

    let scheduler = ReplayScheduler::new_from_encoded(encoded_schedule);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(f);
}
