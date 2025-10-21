//! This is the "inner" crate implementing [`rand` version 0.8] support for [`Shuttle`].
//! This crate should not be depended on directly, the intended way to use this crate is via
//! the `shuttle-rand` crate and feature flag `shuttle`.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//!
//! [`rand` version 0.8]: <https://crates.io/crates/rand/0.8.5>

// TODO:
// There is a case to be made for having Shuttle offering a random number generation construct which is not modelled on
// `ThreadRng` from `rand`, and then build this crate on top of that. This would allow us to implement for instance
// `StdRng`/`SmallRng` fully, and would also nudge consumers to use this crate rather than Shuttle directly.
// sarsko's opinion is that we should do this when we add a mirror for rand 0.9.
pub use shuttle::rand::*;

pub use rand_orig::{CryptoRng, Error, Rng, RngCore, SeedableRng, distributions, seq};

pub mod rngs {
    use crate::*;
    pub use shuttle::rand::rngs::ThreadRng;

    #[cfg(feature = "small_rng")]
    mod small_rng {
        use crate::rngs::ThreadRng;
        use crate::*;

        #[derive(Debug)]
        pub struct SmallRng(ThreadRng);

        impl RngCore for SmallRng {
            #[inline(always)]
            fn next_u32(&mut self) -> u32 {
                self.0.next_u32()
            }

            #[inline(always)]
            fn next_u64(&mut self) -> u64 {
                self.0.next_u64()
            }

            #[inline(always)]
            fn fill_bytes(&mut self, dest: &mut [u8]) {
                self.0.fill_bytes(dest);
            }

            #[inline(always)]
            fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
                self.0.try_fill_bytes(dest)
            }
        }

        // TODO: Don't implement in terms of ThreadRng and implement these properly
        impl PartialEq for SmallRng {
            fn eq(&self, _other: &Self) -> bool {
                unimplemented!()
            }
        }

        impl Eq for SmallRng {}

        impl Clone for SmallRng {
            fn clone(&self) -> Self {
                SmallRng(ThreadRng::default())
            }
        }

        // NOTE: Doesn't actually use seed
        impl SeedableRng for SmallRng {
            type Seed = [u8; 32];

            #[inline(always)]
            fn from_seed(_seed: Self::Seed) -> Self {
                SmallRng(thread_rng())
            }
        }
    }

    #[cfg(feature = "small_rng")]
    pub use small_rng::*;

    #[derive(Debug)]
    pub struct StdRng(ThreadRng);

    // TODO: Don't implement in terms of ThreadRng and implement these properly
    impl PartialEq for StdRng {
        fn eq(&self, _other: &Self) -> bool {
            unimplemented!()
        }
    }

    impl Eq for StdRng {}

    impl Clone for StdRng {
        fn clone(&self) -> Self {
            StdRng(ThreadRng::default())
        }
    }

    impl RngCore for StdRng {
        #[inline(always)]
        fn next_u32(&mut self) -> u32 {
            self.0.next_u32()
        }

        #[inline(always)]
        fn next_u64(&mut self) -> u64 {
            self.0.next_u64()
        }

        #[inline(always)]
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            self.0.fill_bytes(dest);
        }

        #[inline(always)]
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
            self.0.try_fill_bytes(dest)
        }
    }

    // NOTE: Doesn't actually use seed
    impl SeedableRng for StdRng {
        type Seed = [u8; 32];

        #[inline(always)]
        fn from_seed(_seed: Self::Seed) -> Self {
            StdRng(thread_rng())
        }
    }
}

pub mod prelude {
    pub use crate::distributions::Distribution;

    #[cfg(feature = "small_rng")]
    pub use crate::rngs::SmallRng;

    pub use crate::rngs::StdRng;

    pub use crate::seq::{IteratorRandom, SliceRandom};

    pub use crate::random;
    pub use crate::rngs::ThreadRng;
    pub use crate::thread_rng;

    pub use crate::{CryptoRng, Rng, RngCore, SeedableRng};
}

pub fn random<T>() -> T
where
    crate::distributions::Standard: crate::prelude::Distribution<T>,
{
    thread_rng().r#gen()
}
