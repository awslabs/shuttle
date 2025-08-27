use crate::runtime::task::{Task, TaskId};
use crate::scheduler::data::random::RandomDataSource;
use crate::scheduler::data::DataSource;
use crate::scheduler::{Schedule, Scheduler};
use crate::seed_from_env;
use rand::rngs::OsRng;
use rand::seq::SliceRandom;
use rand::{RngCore, SeedableRng};
use rand_pcg::Pcg64Mcg;

/// A scheduler that randomly chooses a runnable task at each context switch.
///
/// The RNG used is contained within the scheduler, allowing it to be reused across executions in
/// order to get different random schedules each time. The scheduler has an optional bias towards
/// remaining on the current task.
#[derive(Debug)]
pub struct RandomScheduler {
    max_iterations: usize,
    rng: Pcg64Mcg,
    iterations: usize,
    data_source: RandomDataSource,
    current_seed: CurrentSeedDropGuard,
}

#[derive(Debug, Default)]
struct CurrentSeedDropGuard {
    inner: Option<u64>,
}

impl CurrentSeedDropGuard {
    fn clear(&mut self) {
        self.inner = None
    }

    fn update(&mut self, seed: u64) {
        self.inner = Some(seed)
    }
}

impl Drop for CurrentSeedDropGuard {
    fn drop(&mut self) {
        if let Some(s) = self.inner {
            eprintln!(
                "failing seed:\n\"\n{s}\n\"\nTo replay the failure, either:\n    1) pass the seed to `shuttle::check_random_with_seed, or\n    2) set the environment variable SHUTTLE_RANDOM_SEED to the seed and run `shuttle::check_random`."
            )
        }
    }
}

impl RandomScheduler {
    /// Construct a new RandomScheduler with a freshly seeded RNG.
    pub fn new(max_iterations: usize) -> Self {
        Self::new_from_seed(OsRng.next_u64(), max_iterations)
    }

    /// Construct a new RandomScheduler with a given seed.
    ///
    /// Two RandomSchedulers initialized with the same seed will make the same scheduling decisions
    /// when executing the same workloads.
    ///
    /// If the `SHUTTLE_RANDOM_SEED` environment variable is set, then that seed will be used instead.
    pub fn new_from_seed(seed: u64, max_iterations: usize) -> Self {
        let seed = seed_from_env(seed);

        let rng = Pcg64Mcg::seed_from_u64(seed);

        Self {
            max_iterations,
            rng,
            iterations: 0,
            data_source: RandomDataSource::initialize(seed),
            current_seed: CurrentSeedDropGuard::default(),
        }
    }
}

impl Scheduler for RandomScheduler {
    fn new_execution(&mut self) -> Option<Schedule> {
        if self.iterations >= self.max_iterations {
            self.current_seed.clear();
            None
        } else {
            self.iterations += 1;
            let seed = self.data_source.reinitialize();
            self.rng = Pcg64Mcg::seed_from_u64(seed);
            self.current_seed.update(seed);
            Some(Schedule::new(seed))
        }
    }

    fn next_task<'a>(
        &mut self,
        runnable: &'a [&'a Task],
        _current: Option<TaskId>,
        _is_yielding: bool,
    ) -> Option<&'a Task> {
        Some(*runnable.choose(&mut self.rng).unwrap())
    }

    fn next_u64(&mut self) -> u64 {
        self.data_source.next_u64()
    }
}
