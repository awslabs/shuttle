//! Shuttle's implementation of the [`rand`] crate, v0.8.
//!
//! Shuttle captures and controls the nondeterminism introduced by randomness and allows it to be
//! replayed deterministically.
//!
//! [`rand`]: https://docs.rs/rand/0.8.5/rand/index.html

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
    ///
    /// A clone draws its own values from the shared RNG. The value that a generator from
    /// [`try_thread_rng`](super::try_thread_rng) holds stays with that generator.
    #[derive(Debug, Default)]
    pub struct ThreadRng {
        // The value that `try_thread_rng` drew to find out whether the scheduler provides random
        // data, which this generator returns first.
        pub(super) first: Option<u64>,
    }

    impl Clone for ThreadRng {
        fn clone(&self) -> Self {
            // All generators share the one RNG, from which the value in `first` was drawn once, for
            // this generator; a copy would hand it out twice.
            Self { first: None }
        }
    }

    impl RngCore for ThreadRng {
        #[inline]
        fn next_u32(&mut self) -> u32 {
            self.next_u64() as u32
        }

        #[inline]
        fn next_u64(&mut self) -> u64 {
            self.first.take().unwrap_or_else(ExecutionState::next_u64)
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
///
/// A draw from it fails the test if the scheduler provides no random data, as under
/// [`check_dfs`](crate::check_dfs). Code that can do without random data takes
/// [`try_thread_rng`] instead.
pub fn thread_rng() -> rngs::ThreadRng {
    rngs::ThreadRng { first: None }
}

/// Retrieve the thread-local random number generator if the scheduler provides random data, or
/// `None` if it does not, as under [`check_dfs`](crate::check_dfs), where a draw from
/// [`thread_rng`] fails the test.
///
/// This is for code that can do without random data, such as a wrapper of a crate that reads the
/// operating system's randomness or clock: it makes a fixed choice of its own when it gets `None`,
/// so that it also runs under `check_dfs`. Under any other scheduler it gets the generator.
///
/// ```
/// use shuttle::rand::{try_thread_rng, Rng};
///
/// // A clock that moves on by 0 or 1 ns per reading, as Shuttle chooses, or by exactly 1 ns where
/// // the scheduler makes no random choices.
/// fn step() -> u64 {
///     match try_thread_rng() {
///         Some(mut rng) => u64::from(rng.gen_bool(0.5)),
///         None => 1,
///     }
/// }
///
/// shuttle::check_dfs(|| assert_eq!(step(), 1), None);
/// shuttle::check_random(|| assert!(step() <= 1), 10);
/// ```
///
/// Finding out whether the scheduler provides random data draws the generator's first value, which
/// the returned generator hands out on its first draw. So asking is a random step of the schedule
/// exactly when the answer is `Some`, and adds nothing to it when it is `None`, and a replay of the
/// schedule gives the same answer at the same point, with the same values. A generator dropped
/// unused has still taken that step. During execution teardown this returns the generator whatever
/// the scheduler, as destructors draw from a generator of teardown's own then.
///
/// # Panics
///
/// Panics outside of a Shuttle test, as a draw from [`thread_rng`] does.
pub fn try_thread_rng() -> Option<rngs::ThreadRng> {
    crate::runtime::execution::ExecutionState::try_next_u64().map(|first| rngs::ThreadRng { first: Some(first) })
}

pub use rand::{Rng, RngCore};
