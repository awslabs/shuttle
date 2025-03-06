use crate::scheduler::data::DataSource;
use crate::scheduler::data::random::RandomDataSource;

/// A `FixedDataSource` generates the same stream of non-determinism (from an underlying
/// `RandomDataSource`) on every execution.
#[derive(Debug)]
pub(crate) struct FixedDataSource {
    seed: u64,
    data_source: RandomDataSource,
}

impl FixedDataSource {
    fn new_from_seed(seed: u64) -> Self {
        Self {
            seed,
            data_source: RandomDataSource::initialize(seed),
        }
    }
}

impl DataSource for FixedDataSource {
    type Seed = u64;

    fn initialize(seed: Self::Seed) -> Self {
        Self::new_from_seed(seed)
    }

    fn reinitialize(&mut self) -> Self::Seed {
        self.data_source = RandomDataSource::initialize(self.seed);
        self.data_source.reinitialize()
    }

    fn next_u64(&mut self) -> u64 {
        self.data_source.next_u64()
    }
}
