use crate::scheduler::data::DataSource;
use rand::{RngCore, SeedableRng};
use rand_pcg::Pcg64Mcg;

/// A `RandomDataSource` generates non-deterministic data from a random number generator, and
/// arranges to re-seed that RNG on each new execution to enable deterministic replay.
#[derive(Debug)]
pub(crate) struct RandomDataSource {
    rng: Pcg64Mcg,
    next_seed: Option<u64>,
}

impl RandomDataSource {
    fn new_from_seed(seed: u64) -> Self {
        Self {
            rng: Pcg64Mcg::seed_from_u64(seed),
            next_seed: Some(seed),
        }
    }
}

impl DataSource for RandomDataSource {
    type Seed = u64;

    fn initialize(seed: Self::Seed) -> Self {
        Self::new_from_seed(seed)
    }

    fn reinitialize(&mut self) -> Self::Seed {
        let next_seed = self.next_seed.take().unwrap_or_else(|| self.rng.next_u64());
        self.rng = Pcg64Mcg::seed_from_u64(next_seed);

        next_seed
    }

    fn next_u64(&mut self) -> u64 {
        self.rng.next_u64()
    }
}
