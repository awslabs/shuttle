//! This crate contains Shuttle's internal implementations of the `parking_lot` crate.
//! Do not depend on this crate directly. Use the `shuttle-parking_lot` crate, which conditionally
//! exposes these implementations with the `shuttle` feature or the original crate without it.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//!
//! [`parking_lot`]: <https://crates.io/crates/parking_lot>

mod mutex;
mod rwlock;

pub use mutex::*;
pub use rwlock::*;
