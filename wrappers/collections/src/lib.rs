//! This crate provides determinizable hash collections.
//!
//! If used without passing any feature flags, then `std::collections::{HashMap, HashSet}`
//! will be used. If the `deterministic` feature flag is set, then a version of `HashMap`
//! and `HashSet` which behaves deterministically will be provided instead.
//!
//! The motivating use case is for [`Shuttle`](https://crates.io/crates/shuttle) testing,
//! where we need iteration order to be deterministic in order to be able to replay tests.
//!
//! This can be done by adding something like the following to your Cargo.toml
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "determinizable-collections/deterministic",
//! ]
//!
//! [dependencies]
//! determinizable-collections = "VERSION_NUMBER"
//! ```
//!
//! It can also be useful to enable it for all tests, by doing the following in your Cargo.toml:
//!
//! ```ignore
//! [dev-dependencies]
//! determinizable-collections = { version = "0.1.0", features = deterministic }
//! ```
//!
//! Some applications may want to have deterministic collections in their pre-production environments,
//! in order to make it easier to recreate any failure behaviors.
//!
//! For the same reason there is a case for using deterministic collections in production, but
//! beware that doing so will lose the advantages of regular `HashMap`s and `HashSet`s. This
//! means the loss of HashDoS attack resilience, as well as "incidental" benefits such as iteration
//! ordering specific bugs going away on retry, or that multiple instances running the same code will
//! not access the same resources in the same order.

cfg_if::cfg_if! {
    if #[cfg(feature = "deterministic")] {
        pub use deterministic_collections::*;
    } else {
        pub use std_collections_reexport::*;
    }
}
