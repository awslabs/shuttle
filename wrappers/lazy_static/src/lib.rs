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
//! lazy_static = { package = "shuttle-lazy_static", version = "1.5" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible `lazy_static` can be done via the "shuttle" feature flag.
//!
//! By default the version of `lazy_static` exported is the latest `1.x` version (the same as if you had written `lazy_static = "1"` in your Cargo.toml).
//! If you need to constrain the version of `lazy_static` that you depend on (eg. to pin the version), then this can be done by adding an entry like:
//!
//! ```ignore
//! [dependencies]
//! lazy_static-version-import-dont-use = { package = "lazy_static", version = "=1.5.0" }
//! ```

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_lazy_static_impl::*;
    } else {
        pub use lazy_static::*;
    }
}
