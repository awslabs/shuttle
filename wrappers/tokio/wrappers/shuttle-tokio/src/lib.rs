//! This crate provides a Shuttle-compatible implementation and wrapper for [`tokio`] in order to make it
//! more ergonomic to run a codebase using [`tokio`] under Shuttle.
//!
//! [`tokio`]: <https://crates.io/crates/tokio>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "tokio/shuttle",
//! ]
//!
//! [dependencies]
//! tokio = { package = "shuttle-tokio", version = "VERSION_NUMBER" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible `tokio` can be done via the "shuttle" feature flag.
//!
//! Note that there are some gaps in what is modeled with regards to [`tokio`]. For documention on this, see the
//! "shuttle-tokio-impl-inner" crate.

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_tokio_impl::*;
    } else {
        pub use tokio::*;
    }
}
