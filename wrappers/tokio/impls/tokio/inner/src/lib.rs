//! This is the "impl-inner" crate implementing [tokio] support for [`Shuttle`].
//! This crate should not be depended on directly, the intended way to use this crate is via
//! the `shuttle-tokio` crate and feature flag `shuttle`.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//!
//! [`tokio`]: <https://crates.io/crates/tokio>

use shuttle::scheduler::{PctScheduler, RandomScheduler};
use shuttle::{PortfolioRunner, Runner};
use std::panic;
use tracing_subscriber::util::SubscriberInitExt as _;

pub mod runtime;
pub mod sync;
pub mod task;
pub mod time;

pub use task::spawn;

pub use tokio::pin;

/// Implementation detail of the `select!` macro. This macro is **not**
/// intended to be used as part of the public API and is permitted to
/// change.
#[doc(hidden)]
pub use tokio_macros::select_priv_declare_output_enum;

/// Implementation detail of the `select!` macro. This macro is **not**
/// intended to be used as part of the public API and is permitted to
/// change.
#[doc(hidden)]
pub use tokio_macros::select_priv_clean_pattern;

#[macro_use]
#[doc(hidden)]
pub mod macros;

// TODO: Finish deprecation and move out of ShuttleTokio
#[deprecated = "`default_shuttle_config` is going to be moved out of ShuttleTokio. Removing it will not be treated as a breaking change."]
#[doc(hidden)]
pub fn default_shuttle_config() -> shuttle::Config {
    let mut config = shuttle::Config::new();
    config.stack_size = 0x0008_0000;
    config.max_steps = shuttle::MaxSteps::FailAfter(10_000_000);
    config
}

// Default for the `test` macto expansions
#[doc(hidden)]
pub fn __default_shuttle_config() -> shuttle::Config {
    let mut config = shuttle::Config::new();
    config.stack_size = 0x0008_0000;
    config.max_steps = shuttle::MaxSteps::FailAfter(10_000_000);
    config
}

// This exists so that the test macro can expand to a call to this.
#[doc(hidden)]
pub fn __check<F>(f: F, config: shuttle::Config, max_iterations: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    #[allow(deprecated)]
    check(f, config, max_iterations)
}

/// Helper function that allows failing tests to be easily replayed using Shuttle.
///
/// Overall workflow:
/// 1. Run the failing test under Shuttle to generate a replayable schedule in a specific directory
///    (specified by the environment variable `SHUTTLE_TRACE_DIR`):
///    `$ env SHUTTLE_TRACE_DIR=./mydir cargo test --release --features shuttle-tests -- <test_name>`
///    This will generate a schedule file ./mydir/schedule000.txt
/// 2. To replay the failure, rerun the test with the environment variable `SHUTTLE_TRACE_FILE` set to
///    the schedule file:
///    `$ env RUST_BACKTRACE=1 SHUTTLE_TRACE_FILE=./mydir/schedule000.txt cargo test --release --features shuttle-tests -- <test_name>`
///
/// Alternatively, if you already have a failing schedule string printed out by Shuttle (e.g., from
/// a dry-run failure) save the schedule string to a file, and run step 2 above with
/// `SHUTTLE_TRACE_FILE` pointing to that file.
///
/// This function also initializes a tracing subscriber, if none has been set yet.
///
/// The following environment variables influence Shuttle execution:
/// - `SHUTTLE_ITERATIONS` sets the number of iterations to run, overriding whatever default is set
///   in the test itself.
/// - `SHUTTLE_TIMEOUT_SECS` sets a time limit (in seconds) to run each test for. If set this will
///   ignore both `SHUTTLE_ITERATIONS` and the test's default iteration count, and instead run the
///   test as many times as possible within the given timeout.
/// - `SHUTTLE_PCT_MAX_DEPTH` sets the maximum depth parameter of the PCT scheduler, overriding the
///   default value 3.
/// - `SHUTTLE_SCHEDULER` sets the scheduler that Shuttle uses.  The value can be either `PCT` (for the
///   PCT scheduler), or `PORTFOLIO` (which runs PCT and random in parallel).  Any other value (or if
///   the variable is not defined) causes Shuttle to use the random scheduler.
/// - `SHUTTLE_INTERVAL_TICKS` sets the max number of ticks each `Interval` generates.  If the variable
///   not defined, the value defaults to `usize::MAX` (essentially, each Interval will generate ticks
///   forever).  Setting the value to 0 means Intervals don't generate any ticks.
/// - `SHUTTLE_HIDE_TRACE` initializes a tracing subscriber that swallows everything. This is useful
///   for code that accesses synchronization primitives in tracing statements, which causes schedules
///   to not replay across verbosity levels. The idea is that we can run randomized tests with something
///   like `RUST_LOG=trace SHUTTLE_HIDE_TRACE=true` without drowning in log messages. A failing schedule
///   can then still be replayed with `RUST_LOG=trace`, giving access to all log messages for debugging.
// TODO: Remove. It's around due to the volume of code still using it and due to `shuttle_tokio::test` being built on it.
#[deprecated = "`check` is going to be moved out of ShuttleTokio. Removing it will not be treated as a breaking change."]
#[doc(hidden)]
pub fn check<F>(f: F, mut config: shuttle::Config, max_iterations: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    match std::env::var("SHUTTLE_HIDE_TRACE").as_ref().map(String::as_str) {
        Ok("true" | "1") => {
            _ = tracing_subscriber::fmt::Subscriber::builder()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .with_writer(std::io::sink)
                .finish()
                .try_init();
        }
        _ => _ = tracing_subscriber::fmt::try_init(),
    }

    if let Ok(path) = std::env::var("SHUTTLE_TRACE_FILE") {
        // Don't spew the schedule out; it's already in a file!
        config.failure_persistence = shuttle::FailurePersistence::None;

        let scheduler = shuttle::scheduler::ReplayScheduler::new_from_file(path).expect("could not read schedule file");
        let runner = shuttle::Runner::new(scheduler, config);
        runner.run(f);
    } else {
        if let Ok(path) = std::env::var("SHUTTLE_TRACE_DIR") {
            config.failure_persistence = shuttle::FailurePersistence::File(Some(std::path::PathBuf::from(path)));
        }

        let max_iterations = if let Some(timeout) = std::env::var("SHUTTLE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            config.max_time = Some(std::time::Duration::from_secs(timeout));
            usize::MAX
        } else {
            std::env::var("SHUTTLE_ITERATIONS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(max_iterations)
        };

        let max_depth = std::env::var("SHUTTLE_PCT_MAX_DEPTH")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(3);

        match std::env::var("SHUTTLE_SCHEDULER") {
            Ok(s) if s == "PORTFOLIO" => {
                let mut runner = PortfolioRunner::new(true, config);
                runner.add(RandomScheduler::new(max_iterations));
                runner.add(PctScheduler::new(max_depth, max_iterations));
                runner.run(f);
            }
            Ok(s) if s == "PCT" => {
                let scheduler = PctScheduler::new(max_depth, max_iterations);
                std::thread::spawn(|| Runner::new(scheduler, config).run(f))
                    .join()
                    .unwrap_or_else(|e| panic::resume_unwind(e));
            }
            _ => {
                let scheduler = RandomScheduler::new(max_iterations);
                std::thread::spawn(|| Runner::new(scheduler, config).run(f))
                    .join()
                    .unwrap_or_else(|e| panic::resume_unwind(e));
            }
        }
    }
}

/// Run the given function under a scheduler that checks whether the function
/// contains randomness which is not controlled by Shuttle.
/// Each iteration will check a different random schedule and replay that schedule once.
#[deprecated = "`check` is going to be moved out of ShuttleTokio. Removing it will not be treated as a breaking change."]
#[doc(hidden)]
pub fn check_for_uncontrolled_nondeterminism<F>(f: F, config: shuttle::Config, max_iterations: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    use shuttle::scheduler::UncontrolledNondeterminismCheckScheduler;
    let _ = tracing_subscriber::fmt::try_init();

    let scheduler = UncontrolledNondeterminismCheckScheduler::new(RandomScheduler::new(max_iterations));

    std::thread::spawn(|| Runner::new(scheduler, config).run(f))
        .join()
        .unwrap_or_else(|e| panic::resume_unwind(e));
}

// Note that we are just pubbing `test`, and not `main`. `main` and `test` share the same code path, so
// both should work, but `main` has not been tested, and we have also yet to experience a need for running
// the `main` function under Shuttle.
pub use tokio_macros::test;
