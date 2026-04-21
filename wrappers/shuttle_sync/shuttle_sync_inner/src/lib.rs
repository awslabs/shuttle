//! This crate contains Shuttle's internal implementation of `std::sync` primitives.
//! Do not depend on this crate directly. Use the `shuttle-sync` crate, which conditionally
//! exposes this implementation with the `shuttle` feature or the original `std::sync` without it.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>

pub mod sync {
    pub use shuttle::sync::*;
}
