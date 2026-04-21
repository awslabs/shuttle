//! This crate provides a Shuttle-compatible wrapper for [`std::sync`] in order to make it
//! more ergonomic to run a codebase under Shuttle.
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "shuttle-sync/shuttle",
//! ]
//!
//! [dependencies]
//! shuttle-sync = "VERSION_NUMBER"
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible `sync` can be done via the "shuttle" feature flag.
//!
//! Note that this crate reexports the entirety of `std::sync`, which contains
//! functionality not present in `shuttle::sync`. This means that the "naive"
//! approach of swapping out all occurrences of `std::sync` is likely to result
//! in not found errors once the "shuttle" feature flag is enabled. The missing
//! functionality will either have to be always gotten from `std`, ie by importing
//! them directly from `std::sync`, or support for the functionality will have
//! to be added to Shuttle.

pub mod sync {
    cfg_if::cfg_if! {
        if #[cfg(feature = "shuttle")] {
            pub use shuttle_sync_inner::sync::*;
        } else {
            pub use std::sync::*;
        }
    }
}
