//! This crate provides a Shuttle-compatible implementation and wrapper for [`tokio-util`] in order to make it
//! more ergonomic to run a codebase using [`tokio-util`] under Shuttle.
//!
//! [`tokio-util`]: <https://crates.io/crates/tokio-util>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "tokio-util/shuttle",
//! ]
//!
//! [dependencies]
//! tokio-util = { package = "shuttle-tokio-util", version = "VERSION_NUMBER" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible `tokio-util`
//! can be done via the "shuttle" feature flag.

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_tokio_util_impl::*;
    } else {
        pub use tokio_util::*;
    }
}
