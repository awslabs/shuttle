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

pub mod sync;
pub mod thread;

pub mod scheduler;

mod runtime;

pub use runtime::runner::Runner;

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
