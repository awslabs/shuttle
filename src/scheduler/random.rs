use crate::runtime::task::TaskId;
use crate::scheduler::Scheduler;
use rand::rngs::OsRng;
use rand::seq::SliceRandom;
use rand::{Rng, RngCore, SeedableRng};
use rand_pcg::Pcg64Mcg;

/// A scheduler that randomly chooses a runnable task at each context switch.
///
/// The RNG used is contained within the scheduler, allowing it to be reused across executions in
/// order to get different random schedules each time. The scheduler has an optional bias towards
/// remaining on the current task.
#[derive(Debug)]
pub struct RandomScheduler {
    max_iterations: usize,
    // Probability in [0, 1]
    bias: f64,
    rng: Pcg64Mcg,
    iterations: usize,
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
            bias: 0.0,
            rng,
            iterations: 0,
        }
    }

    /// Set the scheduler's bias towards remaining on the current task at each context switch.
    ///
    /// The bias is a percentage between 0 and 100 (inclusive). A 100% bias means the scheduler will
    /// always choose to remain on the current task as long as it remains runnable.
    pub fn set_bias(&mut self, bias: f64) {
        assert!(bias <= 1.0);
        self.bias = bias;
    }
}

impl Scheduler for RandomScheduler {
    fn new_execution(&mut self) -> bool {
        if self.iterations >= self.max_iterations {
            false
        } else {
            self.iterations += 1;
            true
        }
    }

    fn next_task(&mut self, runnable: &[TaskId], current: Option<TaskId>) -> TaskId {
        if let Some(current) = current {
            if runnable.contains(&current) && self.rng.gen::<f64>() < self.bias {
                return current;
            }
        }
        *runnable.choose(&mut self.rng).unwrap()
    }
}
