//! This crate provides a Shuttle-compatible implementation and wrapper for [`rand` version 0.10] in order to make it
//! more ergonomic to run a codebase under Shuttle.
//!
//! [`rand` version 0.10]: <https://crates.io/crates/rand/0.10.3>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "rand/shuttle",
//! ]
//!
//! [dependencies]
//! rand = { package = "shuttle-rand_0_10", version = "0.10" }
//! ```
//!
//! The rest of the codebase then remains unchanged. Without the `shuttle` feature this crate is rand. With it,
//! every way rand has of reaching entropy draws from Shuttle's random number generator instead: `rng()`,
//! `random()`, `random_iter()`, `random_range()`, `random_bool()`, `random_ratio()`, `fill()`, `make_rng()`,
//! `rngs::ThreadRng` and `rngs::SysRng`. Seeded generators such as `StdRng::seed_from_u64` stay rand's own. See
//! the [inner crate](https://crates.io/crates/shuttle-rand_0_10-inner) for the details and the limitations.

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_rand_inner::*;
    } else {
        pub use rand_orig::*;
    }
}
