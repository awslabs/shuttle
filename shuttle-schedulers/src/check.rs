use shuttle_core::{Config, Runner};

#[cfg(feature = "annotation")]
use crate::AnnotationScheduler;
use crate::{
    DfsScheduler, PctScheduler, RandomScheduler, ReplayScheduler, RoundRobinScheduler,
    UncontrolledNondeterminismCheckScheduler, UrwRandomScheduler,
};

/// Run the given function once under a round-robin concurrency scheduler.
#[doc(hidden)]
pub fn check<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let runner = Runner::new(RoundRobinScheduler::new(1), Default::default());
    runner.run(f);
}

/// Run the given function under a *uniformly* random scheduler for some number of iterations.
/// Each iteration will run a (potentially) different randomized schedule.
pub fn check_urw<F>(f: F, iterations: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    let runner = Runner::new(UrwRandomScheduler::new(iterations), Default::default());
    runner.run(f);
}

/// Run the given function under a randomized concurrency scheduler for some number of iterations.
/// Each iteration will run a (potentially) different randomized schedule.
pub fn check_random<F>(f: F, iterations: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    let runner = Runner::new(RandomScheduler::new(iterations), Default::default());
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
/// [`ReplayScheduler::new_from_encoded`].
pub fn replay<F>(f: F, encoded_schedule: &str)
where
    F: Fn() + Send + Sync + 'static,
{
    let scheduler = ReplayScheduler::new_from_encoded(encoded_schedule);
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
/// [`ReplayScheduler::new_from_file`].
pub fn replay_from_file<F, P>(f: F, path: P)
where
    F: Fn() + Send + Sync + 'static,
    P: AsRef<std::path::Path>,
{
    let scheduler = ReplayScheduler::new_from_file(path).expect("could not load schedule from file");
    let runner = Runner::new(scheduler, Default::default());
    runner.run(f);
}

/// Run the given function according to a given encoded schedule, while recording an annotated
/// schedule, for use with the Shuttle Explorer extension.
///
/// This function allows deterministic replay of a failing schedule, as long as `f` contains no
/// non-determinism other than that introduced by scheduling.
///
/// This is a convenience function for constructing a [`Runner`] that uses
/// an [`AnnotationScheduler`] wrapping a replay scheduler created with
/// [`ReplayScheduler::new_from_encoded`].
#[cfg(feature = "annotation")]
pub fn annotate_replay<F>(f: F, encoded_schedule: &str)
where
    F: Fn() + Send + Sync + 'static,
{
    let scheduler_inner = ReplayScheduler::new_from_encoded(encoded_schedule);
    let scheduler = AnnotationScheduler::new(scheduler_inner);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(f);
}
