use crate::runtime::task::TaskId;
use crate::scheduler::data::random::RandomDataSource;
use crate::scheduler::data::DataSource;
use crate::scheduler::{Schedule, Scheduler};
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
    pub fn new_from_seed(seed: u64, max_iterations: usize) -> Self {
        let rng = Pcg64Mcg::seed_from_u64(seed);
        Self {
            max_iterations,
            rng,
            iterations: 0,
            data_source: RandomDataSource::initialize(seed),
        }
    }
}

impl Scheduler for RandomScheduler {
    fn new_execution(&mut self) -> Option<Schedule> {
        if self.iterations >= self.max_iterations {
            None
        } else {
            self.iterations += 1;
            Some(Schedule::new(self.data_source.reinitialize()))
        }
    }

    fn next_task(&mut self, runnable: &[TaskId], _current: Option<TaskId>) -> Option<TaskId> {
        Some(*runnable.choose(&mut self.rng).unwrap())
    }

    fn next_u64(&mut self) -> u64 {
        self.data_source.next_u64()
    }
}
