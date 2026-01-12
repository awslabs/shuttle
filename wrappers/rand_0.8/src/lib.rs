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
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible `rand` can be done via the "shuttle" feature flag.
//!
//! By default the version of `rand` exported is the latest `0.8` version (the same as if you had written `rand = "0.8"` in your Cargo.toml).
//! If you need to constrain the version of `rand` that you depend on (eg. to pin the version), then this can be done by adding an entry like:
//!
//! ```ignore
//! [dependencies]
//! rand-version-import-dont-use = { package = "rand", version = "=0.8.5" }
//! ```

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_rand_impl::*;
    } else {
        pub use rand::*;
    }
}
