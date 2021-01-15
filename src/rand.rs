//! Shuttle's implementation of the `rand` crate. Shuttle captures and controls the nondeterminism
//! introduced by randomness and allows it to be replayed deterministically.

/// Random number generators and adapters
pub mod rngs {
    use crate::runtime::execution::ExecutionState;
    use rand::{CryptoRng, RngCore};
    use rand_core::impls::fill_bytes_via_next;

    /// A reference to the thread-local generator
    ///
    /// An instance can be obtained via `thread_rng()` or via `ThreadRng::default()`. Note that
    /// unlike in the `rand` crate, this RNG is not *actually* thread-local --- all threads share a
    /// single RNG. This RNG is automatically seeded by Shuttle, and cannot be re-seeded, so this
    /// sharing should be indistinguishable from truly thread-local behavior.
    #[derive(Debug, Default)]
    pub struct ThreadRng;

    impl RngCore for ThreadRng {
        #[inline]
        fn next_u32(&mut self) -> u32 {
            self.next_u64() as u32
        }

        #[inline]
        fn next_u64(&mut self) -> u64 {
            ExecutionState::next_u64()
        }

        fn fill_bytes(&mut self, dest: &mut [u8]) {
            fill_bytes_via_next(self, dest)
        }

        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    impl CryptoRng for ThreadRng {}
}

/// Retrieve the thread-local random number generator, seeded by the system. Intended to be used in
/// method chaining style, e.g. `thread_rng().gen::<i32>()`, or cached locally, e.g.
/// `let mut rng = thread_rng();`.
pub fn thread_rng() -> rngs::ThreadRng {
    rngs::ThreadRng
}

pub use rand::{Rng, RngCore};
