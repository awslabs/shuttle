//! This crate provides a Shuttle-compatible implementation and wrapper for [`tokio-retry`] in order to make it
//! more ergonomic to run a codebase using [`tokio-retry`] under Shuttle.
//!
//! [`tokio-retry`]: <https://crates.io/crates/tokio-retry>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "tokio-retry/shuttle",
//! ]
//!
//! [dependencies]
//! tokio-retry = { package = "shuttle-tokio-retry", version = "0.3" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible `tokio-retry`
//! can be done via the "shuttle" feature flag.

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_tokio_retry_impl::*;
    } else {
        pub use tokio_retry_orig::*;
    }
}
