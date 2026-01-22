//! This crate contains Shuttle's internal implementations of the `lazy_static` crate.
//! Do not depend on this crate directly. Use the `shuttle-lazy_static` crate, which conditionally
//! exposes these implementations with the `shuttle` feature or the original crate without it.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//!
//! [`lazy_static`]: <https://crates.io/crates/lazy_static>

// The reason this crate exists and we don't just import directly from shuttle in the wrapper is
// that by doing it like this we can change where the implementation is sourced from without changing
// the wrapper, which is a property needed if one wants the wrapper version to match the version of
// `lazy_static`
pub use shuttle::lazy_static::*;
