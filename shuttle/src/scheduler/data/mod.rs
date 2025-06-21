use std::fmt::Debug;

pub(crate) mod fixed;
pub(crate) mod random;
pub use random::RandomDataSource;

/// A `DataSource` is an oracle for generating non-deterministic data to return to a task that asks
/// for random values.
pub trait DataSource: Debug {
    /// A `Seed` can be used to deterministically initialize a data source such that it produces the
    /// same sequence of outputs for `next_u64` calls.
    type Seed: Clone;

    /// Initialize the `DataSource` from a given seed.
    fn initialize(seed: Self::Seed) -> Self;

    /// Reinitialize the `DataSource` and return a seed that can be used to return to this state
    /// for deterministic replay.
    fn reinitialize(&mut self) -> Self::Seed;

    /// Generate the next non-deterministic `u64` value to return to a requesting task.
    fn next_u64(&mut self) -> u64;
}
