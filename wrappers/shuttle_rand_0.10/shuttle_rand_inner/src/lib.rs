//! This is the "inner" crate implementing [`rand` version 0.10] support for [`Shuttle`].
//! This crate should not be depended on directly, the intended way to use this crate is via
//! the `shuttle-rand_0_10` crate and feature flag `shuttle`.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//! [`rand` version 0.10]: <https://crates.io/crates/rand/0.10.3>
//!
//! # Where the randomness comes from
//!
//! Every way rand 0.10 has of reaching entropy draws from Shuttle's random number generator here instead.
//! Shuttle records each value it hands out in the schedule, so a failing execution replays with the same
//! values, and `shuttle::check_uncontrolled_nondeterminism` does not report them:
//!
//! - [`rng()`] and the [`rngs::ThreadRng`] it returns (also through `ThreadRng::default()`);
//! - [`random()`], [`random_iter()`], [`random_range()`], [`random_bool()`], [`random_ratio()`] and [`fill()`],
//!   which use that handle as rand's do;
//! - [`make_rng()`], which seeds the generator it returns from that handle, as rand's does;
//! - [`rngs::SysRng`], rand's name for `getrandom::SysRng`.
//!
//! rand 0.10 has no `SeedableRng::from_os_rng` (it became `make_rng`), so [`SeedableRng`] is rand_core's own
//! trait, as in rand. Seeded generators stay rand's own too: `rngs::StdRng`, `rngs::SmallRng`, the xoshiro and
//! the ChaCha generators are rand's types, and `StdRng::seed_from_u64(7)` produces exactly the values it produces
//! with rand. So do the distributions in [`distr`], the sequence functions in [`seq`], and the [`Rng`],
//! [`RngExt`] and [`TryRng`] traits, all of which are rand's own and take their randomness from the generator
//! they are given.
//!
//! ```
//! use shuttle_rand_0_10_inner as rand;
//! use rand::RngExt;
//!
//! shuttle::check_random(
//!     || {
//!         let roll: u8 = rand::random_range(1..=6);
//!         assert!((1..=6).contains(&roll));
//!         let mut seeded: rand::rngs::StdRng = rand::make_rng();
//!         let _: u64 = seeded.random();
//!     },
//!     10,
//! );
//! ```
//!
//! # Limitations
//!
//! - **Outside of a Shuttle test, the entry points above panic**, as the rest of Shuttle does. The seeded
//!   generators, the distributions and the sequence functions work anywhere.
//! - **`check_dfs` refuses them.** `shuttle::check_dfs` builds its scheduler with `allow_random_data = false`,
//!   and every entry point above draws from Shuttle's generator, so such a test panics with "requested random
//!   data from DFS scheduler", as with `shuttle::rand`. `shuttle::scheduler::DfsScheduler::new(_, true)` with a
//!   `Runner` gives every execution the same values instead. A generator seeded with a fixed seed draws nothing
//!   from Shuttle, so it works under `check_dfs`.
//! - **No scheduling points.** Drawing a value does not switch tasks, as reading rand's thread-local generator
//!   does not synchronize with anything.
//! - **`getrandom` reached directly is getrandom's own.** `getrandom::SysRng`, `getrandom::fill` and the like
//!   read the operating system; only rand's own path, `rand::rngs::SysRng`, is this crate's.
//! - **[`rngs::ThreadRng::reseed`] does nothing**, since Shuttle's generator cannot be reseeded.
//! - **Types.** `ThreadRng` and `SysRng` are this crate's own, so a function of another crate that takes rand's
//!   `ThreadRng` or `SysRng` by name does not take these. A function generic over `Rng` or `TryRng` takes both.

#![deny(missing_docs, missing_debug_implementations)]

pub use rand_orig::{
    CryptoRng, Fill, Rng, RngExt, RngReader, SeedableRng, TryCryptoRng, TryRng, distr, rand_core, seq,
};

pub mod prelude;
pub mod rngs;
mod source;

use crate::distr::{Distribution, StandardUniform};

/// Access a handle to Shuttle's random number generator, in place of rand's thread-local generator.
///
/// See [`rngs::ThreadRng`]. Panics outside of a Shuttle test.
pub fn rng() -> rngs::ThreadRng {
    rngs::ThreadRng::new()
}

/// Construct and seed an RNG
///
/// This function yields a generator seeded from [`rng`] ([`rngs::ThreadRng`]), as rand's does when its
/// `thread_rng` feature is on, and so from Shuttle's random number generator.
///
/// # Examples
///
/// ```
/// # shuttle::check_random(|| {
/// use shuttle_rand_0_10_inner as rand;
/// let mut rng: rand::rngs::SmallRng = rand::make_rng();
/// # let _ = rand::Rng::next_u32(&mut rng);
/// # }, 1);
/// ```
///
/// # Panics
///
/// Outside of a Shuttle test.
#[track_caller]
pub fn make_rng<R: SeedableRng>() -> R {
    R::from_rng(&mut rng())
}

/// Generate a random value using Shuttle's random number generator.
///
/// This function is shorthand for <code>[rng()].[random()](RngExt::random)</code>. See
/// [`StandardUniform`] for the supported types.
#[inline]
pub fn random<T>() -> T
where
    StandardUniform: Distribution<T>,
{
    rng().random()
}

/// Return an iterator over [`random()`] variates, drawn from Shuttle's random number generator.
///
/// This function is shorthand for <code>[rng()].[random_iter](RngExt::random_iter)()</code>.
#[inline]
pub fn random_iter<T>() -> distr::Iter<StandardUniform, rngs::ThreadRng, T>
where
    StandardUniform: Distribution<T>,
{
    rng().random_iter()
}

/// Generate a random value in the given range using Shuttle's random number generator.
///
/// This function is shorthand for <code>[rng()].[random_range](RngExt::random_range)(<var>range</var>)</code>.
#[inline]
pub fn random_range<T, R>(range: R) -> T
where
    T: distr::uniform::SampleUniform,
    R: distr::uniform::SampleRange<T>,
{
    rng().random_range(range)
}

/// Return a bool with a probability `p` of being true, using Shuttle's random number generator.
///
/// This function is shorthand for <code>[rng()].[random_bool](RngExt::random_bool)(<var>p</var>)</code>.
///
/// # Panics
///
/// If `p < 0` or `p > 1`.
#[inline]
#[track_caller]
pub fn random_bool(p: f64) -> bool {
    rng().random_bool(p)
}

/// Return a bool with a probability of `numerator/denominator` of being true, using Shuttle's random number
/// generator.
///
/// This function is shorthand for
/// <code>[rng()].[random_ratio](RngExt::random_ratio)(<var>numerator</var>, <var>denominator</var>)</code>.
///
/// # Panics
///
/// If `denominator == 0` or `numerator > denominator`.
#[inline]
#[track_caller]
pub fn random_ratio(numerator: u32, denominator: u32) -> bool {
    rng().random_ratio(numerator, denominator)
}

/// Fill any type implementing [`Fill`] with random data from Shuttle's random number generator.
///
/// This function is shorthand for <code>[rng()].[fill](RngExt::fill)(<var>dest</var>)</code>.
#[inline]
#[track_caller]
pub fn fill<T: Fill>(dest: &mut [T]) {
    Fill::fill_slice(dest, &mut rng())
}
