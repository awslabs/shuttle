//! This crate provides a Shuttle-compatible implementation and wrapper for [`rand` version 0.8] in order to make it
//! more ergonomic to run a codebase under Shuttle.
//!
//! [`rand` version 0.8]: <https://crates.io/crates/rand/0.8.5>
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
//! rand = { package = "shuttle-rand", version = "0.8" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-conpatible `rand` can be done via the "shuttle" feature flag.

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_rand_inner::*;
    } else {
        pub use rand_orig::*;
    }
}
