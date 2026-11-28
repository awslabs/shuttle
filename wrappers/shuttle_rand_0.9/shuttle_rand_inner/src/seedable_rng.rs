//! rand's `SeedableRng`, with `from_os_rng` and `try_from_os_rng` seeding from Shuttle.
//!
//! The provided methods are rand_core 0.9's (MIT OR Apache-2.0, Copyright 2018 Developers of the Rand
//! project), so a generator that implements this trait itself behaves as it would with rand_core's.

use crate::source;
use rand_orig::rand_core;
use rand_orig::{RngCore, TryRngCore};

/// A random number generator that can be explicitly seeded: rand's [`SeedableRng`](rand_core::SeedableRng),
/// with its seed taken from Shuttle where rand's takes it from the operating system.
///
/// rand's `SeedableRng` is rand_core's trait, and `from_os_rng` and `try_from_os_rng` are provided methods of
/// it that call `getrandom`. Another crate cannot change those, so this crate has a trait of its own, with the
/// same items, and implements it for every type that implements rand_core's. That implementation forwards
/// [`from_seed`](SeedableRng::from_seed), [`seed_from_u64`](SeedableRng::seed_from_u64),
/// [`from_rng`](SeedableRng::from_rng) and [`try_from_rng`](SeedableRng::try_from_rng) to rand_core's, so a
/// seeded generator such as `StdRng::seed_from_u64(7)` produces exactly what it produces in rand.
/// [`from_os_rng`](SeedableRng::from_os_rng) and [`try_from_os_rng`](SeedableRng::try_from_os_rng) fill the
/// seed from Shuttle's random number generator instead, which records the values in the schedule.
///
/// A generator that implements this trait itself, rather than rand_core's, gets the same provided methods,
/// with rand_core's bodies, except that its `from_os_rng` seeds from Shuttle too.
pub trait SeedableRng: Sized {
    /// Seed type, which is restricted to types mutably-dereferenceable as `u8` arrays (we recommend `[u8; N]`
    /// for some `N`).
    type Seed: Clone + Default + AsRef<[u8]> + AsMut<[u8]>;

    /// Create a new PRNG using the given seed.
    fn from_seed(seed: Self::Seed) -> Self;

    /// Create a new PRNG using a `u64` seed, as rand_core's `seed_from_u64` does.
    ///
    /// This **is not suitable for cryptography**, as should be clear given that the input size is only 64 bits.
    fn seed_from_u64(mut state: u64) -> Self {
        // We use PCG32 to generate a u32 sequence, and copy to the seed
        fn pcg32(state: &mut u64) -> [u8; 4] {
            const MUL: u64 = 6364136223846793005;
            const INC: u64 = 11634580027462260723;

            // We advance the state first (to get away from the input value,
            // in case it has low Hamming Weight).
            *state = state.wrapping_mul(MUL).wrapping_add(INC);
            let state = *state;

            // Use PCG output function with to_le to generate x:
            let xorshifted = (((state >> 18) ^ state) >> 27) as u32;
            let rot = (state >> 59) as u32;
            let x = xorshifted.rotate_right(rot);
            x.to_le_bytes()
        }

        let mut seed = Self::Seed::default();
        let (chunks, rem) = seed.as_mut().as_chunks_mut::<4>();
        for chunk in chunks {
            *chunk = pcg32(&mut state);
        }
        if !rem.is_empty() {
            rem.copy_from_slice(&pcg32(&mut state)[..rem.len()]);
        }

        Self::from_seed(seed)
    }

    /// Create a new PRNG seeded from an infallible `Rng`.
    fn from_rng(rng: &mut impl RngCore) -> Self {
        let mut seed = Self::Seed::default();
        rng.fill_bytes(seed.as_mut());
        Self::from_seed(seed)
    }

    /// Create a new PRNG seeded from a potentially fallible `Rng`.
    fn try_from_rng<R: TryRngCore>(rng: &mut R) -> Result<Self, R::Error> {
        let mut seed = Self::Seed::default();
        rng.try_fill_bytes(seed.as_mut())?;
        Ok(Self::from_seed(seed))
    }

    /// Creates a new instance of the RNG seeded from Shuttle's random number generator, where rand's seeds it
    /// via `getrandom`.
    ///
    /// Two executions with the same schedule get the same generator, and a replay of a failing one gets the one
    /// it had. Panics outside of a Shuttle test.
    fn from_os_rng() -> Self {
        match Self::try_from_os_rng() {
            Ok(res) => res,
            Err(err) => panic!("from_os_rng failed: {}", err),
        }
    }

    /// Creates a new instance of the RNG seeded from Shuttle's random number generator, where rand's seeds it
    /// via `getrandom`. Never fails; panics outside of a Shuttle test.
    fn try_from_os_rng() -> Result<Self, getrandom::Error> {
        let mut seed = Self::Seed::default();
        source::fill_bytes(seed.as_mut());
        Ok(Self::from_seed(seed))
    }
}

impl<T: rand_core::SeedableRng> SeedableRng for T {
    type Seed = <T as rand_core::SeedableRng>::Seed;

    #[inline]
    fn from_seed(seed: Self::Seed) -> Self {
        <T as rand_core::SeedableRng>::from_seed(seed)
    }

    #[inline]
    fn seed_from_u64(state: u64) -> Self {
        <T as rand_core::SeedableRng>::seed_from_u64(state)
    }

    #[inline]
    fn from_rng(rng: &mut impl RngCore) -> Self {
        <T as rand_core::SeedableRng>::from_rng(rng)
    }

    #[inline]
    fn try_from_rng<R: TryRngCore>(rng: &mut R) -> Result<Self, R::Error> {
        <T as rand_core::SeedableRng>::try_from_rng(rng)
    }
}
