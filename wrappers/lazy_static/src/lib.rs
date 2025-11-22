//! This crate provides a Shuttle-compatible implementation and wrapper for [`lazy_static`] in order to make it
//! more ergonomic to run a codebase under Shuttle.
//!
//! [`lazy_static`]: <https://crates.io/crates/lazy_static>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "lazy_static/shuttle",
//! ]
//!
//! [dependencies]
//! lazy_static = { package = "shuttle-lazy_static", version = "VERSION_NUMBER" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible `lazy_static` can be done via the "shuttle" feature flag.

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_lazy_static_impl::*;
    } else {
        pub use lazy_static::*;
    }
}
