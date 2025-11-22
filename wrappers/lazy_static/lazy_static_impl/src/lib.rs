//! This is the "impl" crate implementing [lazy_static] support for [`Shuttle`].
//! This crate should not be depended on directly, the intended way to use this crate is via
//! the `shuttle-lazy_static` crate and feature flag `shuttle`.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//!
//! [`lazy_static`]: <https://crates.io/crates/lazy_static>

// The reason this crate exists and we don't just import directly from shuttle in the wrapper is
// that by doing it like this we can change where the implementation is sourced from without changing
// the wrapper, which is a property needed if one wants the wrapper version to match the version of
// `lazy_static`
pub use shuttle::lazy_static::*;
