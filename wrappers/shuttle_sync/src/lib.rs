//! This crate provides a wrapper for `{std, shuttle}::sync` in order to make it
//! more ergonomic to run a codebase under Shuttle.
//!
//! This is done by swapping usages of `std::sync` with `shuttle_sync::sync`.
//! If the "shuttle" feature flag is not provided, this will reexport primitives
//! from `std::sync`, and if the "shuttle" feature flag is provided, then the
//! sync primitives from Shuttle will be exported instead.
//!
//! Note that this crate reexports the entirety of `std::sync`, which contains
//! functionality not present in `shuttle::sync`. This means that the "naive"
//! approach of swapping out all occurrences of `std::sync` is likely to result
//! in not found errors once the "shuttle" feature flag is enabled. The missing
//! functionality will either have to be always gotten from `std`, ie by importing
//! them directly from `std::sync`, or support for the functionality will have
//! to be added to Shuttle.

pub mod sync {
    #[cfg(not(feature = "shuttle"))]
    pub use std::sync::*;

    #[cfg(feature = "shuttle")]
    pub use shuttle::sync::*;
}
