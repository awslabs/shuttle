//! This crate provides a Shuttle-compatible implementation and wrapper for [`fastrand`] in order to make it more
//! ergonomic to run a codebase under Shuttle.
//!
//! [`fastrand`]: <https://crates.io/crates/fastrand>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "fastrand/shuttle",
//! ]
//!
//! [dependencies]
//! fastrand = { package = "shuttle-fastrand", version = "2" }
//! ```
//!
//! The rest of the codebase then remains unchanged. Without the `shuttle` feature this crate is fastrand. With
//! it, the generator behind fastrand's free functions (`fastrand::u64(..)`, `fastrand::shuffle`, ...) belongs to
//! the running Shuttle task and is seeded from Shuttle's random number generator, and so is the generator that
//! `Rng::new()` forks from it. `Rng::with_seed` stays fastrand's own. See the
//! [inner crate](https://crates.io/crates/shuttle-fastrand-inner) for the details and the limitations.

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_fastrand_inner::*;
    } else {
        pub use fastrand_orig::*;
    }
}
