//! This is the "inner" crate implementing [`rand` version 0.9] support for [`Shuttle`].
//! This crate should not be depended on directly, the intended way to use this crate is via
//! the `shuttle-rand_0_9` crate and feature flag `shuttle`.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//! [`rand` version 0.9]: <https://crates.io/crates/rand/0.9.5>
//!
//! # Where the randomness comes from
//!
//! Every way rand 0.9 has of reaching entropy draws from Shuttle's random number generator here instead.
//! Shuttle records each value it hands out in the schedule, so a failing execution replays with the same
//! values, and `shuttle::check_uncontrolled_nondeterminism` does not report them:
//!
//! - [`rng()`], the deprecated [`thread_rng()`], and the [`rngs::ThreadRng`] they return (also through
//!   `ThreadRng::default()`);
//! - [`random()`], [`random_iter()`], [`random_range()`], [`random_bool()`], [`random_ratio()`] and [`fill()`],
//!   which use that handle as rand's do;
//! - [`rngs::OsRng`];
//! - [`SeedableRng::from_os_rng`] and [`SeedableRng::try_from_os_rng`], which fill the seed from Shuttle's
//!   generator where rand's call `getrandom`.
//!
//! Seeded generators stay rand's own: `rngs::StdRng` and `rngs::SmallRng` are rand's types, and
//! `StdRng::seed_from_u64(7)` produces exactly the values it produces with rand. So do the distributions in
//! [`distr`], the sequence functions in [`seq`], and the [`Rng`] and [`RngCore`] traits, all of which are rand's
//! own and take their randomness from the generator they are given.
//!
//! ```
//! use shuttle_rand_0_9_inner as rand;
//! use rand::{Rng, SeedableRng};
//!
//! shuttle::check_random(
//!     || {
//!         let roll: u8 = rand::random_range(1..=6);
//!         assert!((1..=6).contains(&roll));
//!         let mut seeded = rand::rngs::StdRng::from_os_rng();
//!         let _: u64 = seeded.random();
//!     },
//!     10,
//! );
//! ```
//!
//! # `SeedableRng`
//!
//! rand's `SeedableRng` is rand_core's trait, whose `from_os_rng` is a provided method that this crate cannot
//! change. So [`SeedableRng`] here is a trait of this crate's own with the same items, implemented for every type
//! that implements rand_core's: everything but the two `os_rng` methods forwards to rand_core's implementation.
//! Code that names `rand::SeedableRng`, directly or through the prelude, compiles unchanged. Three things differ
//! from rand, where the two traits are one:
//!
//! - **Both traits in scope.** A module that imports this trait and also rand_core's under another name (say
//!   `use rand::rand_core::SeedableRng as _;`) gets "multiple applicable items" errors for calls like
//!   `StdRng::seed_from_u64`. Import only `rand::SeedableRng`.
//! - **Generators that implement it themselves.** A type that implements `rand::SeedableRng` implements this
//!   trait, not rand_core's, so it cannot be handed to code that requires rand_core's. Implement
//!   `rand::rand_core::SeedableRng` instead, and this trait comes with it.
//! - **Generic bounds.** In a function bounded by `R: rand::SeedableRng`, `R` cannot be handed to another
//!   crate's API that requires rand_core's `SeedableRng`. Bound it by `rand::rand_core::SeedableRng` instead,
//!   which every type this trait covers implements, and seed it with `<R as rand::SeedableRng>::from_os_rng()`:
//!   a plain `R::from_os_rng()` there is rand_core's, which reads the operating system.
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
//! - **`rand_core` reached directly is rand_core's own.** [`rand_core`] is re-exported unchanged, so
//!   `rand::rand_core::OsRng` and the `from_os_rng` of `rand::rand_core::SeedableRng` read the operating system,
//!   as do `getrandom` and any other crate's generators seeded through rand_core's trait.
//! - **[`rngs::ThreadRng::reseed`] does nothing**, since Shuttle's generator cannot be reseeded.
//! - **Types.** `ThreadRng` and `OsRng` are this crate's own, so a function of another crate that takes rand's
//!   `ThreadRng` or `OsRng` by name does not take these. A function generic over `RngCore` or `TryRngCore` takes
//!   both.

#![deny(missing_docs, missing_debug_implementations)]

pub use rand_orig::{CryptoRng, Fill, Rng, RngCore, TryCryptoRng, TryRngCore, distr, rand_core, seq};

pub mod prelude;
pub mod rngs;
mod seedable_rng;
mod source;

pub use seedable_rng::SeedableRng;

use crate::distr::{Distribution, StandardUniform};

/// Access a handle to Shuttle's random number generator, in place of rand's thread-local generator.
///
/// See [`rngs::ThreadRng`]. Panics outside of a Shuttle test.
pub fn rng() -> rngs::ThreadRng {
    rngs::ThreadRng::new()
}

/// Access the thread-local generator
///
/// Use [`rand::rng()`](rng()) instead.
#[deprecated(since = "0.9.0", note = "Renamed to `rng`")]
#[inline]
pub fn thread_rng() -> rngs::ThreadRng {
    rng()
}

/// Generate a random value using Shuttle's random number generator.
///
/// This function is shorthand for <code>[rng()].[random()](Rng::random)</code>. See
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
/// This function is shorthand for <code>[rng()].[random_iter](Rng::random_iter)()</code>.
#[inline]
pub fn random_iter<T>() -> distr::Iter<StandardUniform, rngs::ThreadRng, T>
where
    StandardUniform: Distribution<T>,
{
    rng().random_iter()
}

/// Generate a random value in the given range using Shuttle's random number generator.
///
/// This function is shorthand for <code>[rng()].[random_range](Rng::random_range)(<var>range</var>)</code>.
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
/// This function is shorthand for <code>[rng()].[random_bool](Rng::random_bool)(<var>p</var>)</code>.
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
/// <code>[rng()].[random_ratio](Rng::random_ratio)(<var>numerator</var>, <var>denominator</var>)</code>.
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
/// This function is shorthand for <code>[rng()].[fill](Rng::fill)(<var>dest</var>)</code>.
#[inline]
#[track_caller]
pub fn fill<T: Fill + ?Sized>(dest: &mut T) {
    dest.fill(&mut rng())
}
