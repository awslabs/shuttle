//! This crate provides a Shuttle-compatible implementation and wrapper for [`parking_lot`] in order to make it
//! more ergonomic to run a codebase under Shuttle.
//!
//! [`parking_lot`]: <https://crates.io/crates/parking_lot>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "parking_lot/shuttle",
//! ]
//!
//! [dependencies]
//! parking_lot = { package = "shuttle-parking_lot", version = "0.12" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible `parking_lot` can be done via the "shuttle" feature flag.
//!
//! By default the version of `parking_lot` exported is the latest `0.12` version (the same as if you had written `parking_lot = "0.12"` in your Cargo.toml).
//! If you need to constrain the version of `parking_lot` that you depend on (eg. to pin the version), then this can be done by adding an entry like:
//!
//! ```ignore
//! [dependencies]
//! parking_lot-version-import-dont-use = { package = "parking_lot", version = "=0.12.5" }
//! ```

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_parking_lot_impl::*;
    } else {
        pub use parking_lot::*;
    }
}
