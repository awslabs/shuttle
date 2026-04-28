//! This crate contains Shuttle's internal implementation of the `tokio-retry` crate.
//! Do not depend on this crate directly. Use the `shuttle-tokio-retry` crate, which conditionally
//! exposes this implementation with the `shuttle` feature or the original crate without it.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//!
//! [`tokio-retry`]: <https://crates.io/crates/tokio-retry>

#![allow(warnings)]

mod action;
mod condition;
mod future;
/// Assorted retry strategies including fixed interval and exponential back-off.
pub mod strategy;

pub use action::Action;
pub use condition::Condition;
pub use future::{Retry, RetryIf};
