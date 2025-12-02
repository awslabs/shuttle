//! This crate provides a Shuttle-compatible implementation and wrapper for [`tokio-stream`] in order to make it
//! more ergonomic to run a codebase using [`tokio-stream`] under Shuttle.
//!
//! [`tokio-stream`]: <https://crates.io/crates/tokio-stream>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "tokio-stream/shuttle",
//! ]
//!
//! [dependencies]
//! tokio-stream = { package = "shuttle-tokio-stream", version = "VERSION_NUMBER" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible `shuttle-tokio-stream`
//! can be done via the "shuttle" feature flag.

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_tokio_stream_impl::*;
    } else {
        pub use tokio_stream::*;
    }
}
