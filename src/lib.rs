#![deny(warnings, missing_debug_implementations, missing_docs)]

//! Shuttle is a library for testing concurrent Rust code, heavily inspired by [Loom].
//!
//! Shuttle focuses on unsound systematic testing, rather than the exhaustive testing that Loom
//! offers. This is a soundness--scalability trade-off: Shuttle is not sound (a passing Shuttle test
//! does not prove the code is correct), but it should scale to much larger test cases than Loom.
//! Empirically, systematic testing is successful at finding most concurrency bugs, which tend not
//! to be adversarial.
//!
//! [Loom]: https://github.com/tokio-rs/loom

pub mod asynch;
pub mod rand;
pub mod sync;
pub mod thread;

pub mod scheduler;

mod runtime;

pub use runtime::runner::{PortfolioRunner, Runner};

/// Run the given function once under a round-robin concurrency scheduler.
pub fn check<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    use crate::scheduler::RoundRobinScheduler;

    let runner = Runner::new(RoundRobinScheduler::new());
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
    let runner = Runner::new(scheduler);
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
    let runner = Runner::new(scheduler);
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
    let runner = Runner::new(scheduler);
    runner.run(f);
}
