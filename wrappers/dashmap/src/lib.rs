//! This crate provides a Shuttle-compatible implementation and wrapper for [`dashmap`] in order to make it
//! more ergonomic to run a codebase under Shuttle.
//!
//! [`dashmap`]: <https://crates.io/crates/dashmap>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "dashmap/shuttle",
//! ]
//!
//! [dependencies]
//! dashmap = { package = "shuttle-dashmap", version = "6" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible `dashmap` can be done via the "shuttle" feature flag.

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_dashmap_impl::*;
    } else {
        pub use dashmap::*;
    }
}
