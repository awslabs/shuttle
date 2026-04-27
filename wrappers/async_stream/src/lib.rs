//! This crate provides a Shuttle-compatible implementation and wrapper for [`async-stream`] in order to make it
//! more ergonomic to run a codebase under Shuttle.
//!
//! [`async-stream`]: <https://crates.io/crates/async-stream>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "async-stream/shuttle",
//! ]
//!
//! [dependencies]
//! async-stream = { package = "shuttle-async-stream", version = "0.3" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible `async-stream` can be done via the "shuttle" feature flag.
//!
//! By default the version of `async-stream` exported is the latest `0.3` version (the same as if you had written `async-stream = "0.3"` in your Cargo.toml).
//! If you need to constrain the version of `async-stream` that you depend on (eg. to pin the version), then this can be done by adding an entry like:
//!
//! ```ignore
//! [dependencies]
//! async-stream-version-import-dont-use = { package = "async-stream", version = "=0.3.6" }
//! ```

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_async_stream_impl::*;
    } else {
        pub use async_stream_orig::*;
    }
}
