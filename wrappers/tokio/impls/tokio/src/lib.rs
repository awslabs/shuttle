//! This crate contains Shuttle's internal implementations of the `tokio` crate.
//! Do not depend on this crate directly. Use the `shuttle-tokio` crate, which conditionally
//! exposes these implementations with the `shuttle` feature or the original crate without it.
//!
//! The reason there exists an `impl` crate (this crate) and an `impl-inner` crate is to make it
//! clear which parts are just reexports from tokio, and which parts are made to be shuttle-compatible.
//! Some notable gaps in this regard are `io`, `net` and `fs`, which all exist as reexports just to make
//! it easier to get code to compile under Shuttle, but which will run into issues if they are used in a
//! Shuttle test.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//!
//! [`tokio`]: <https://crates.io/crates/tokio>

pub use shuttle_tokio_impl_inner::*;
pub use tokio_orig::{io, net};

// TODO / WARN: `task_local` needs to be implemented in Shuttle. Currently not correct, and gives shared storage for all `Task`s
#[cfg(feature = "rt")]
pub use tokio_orig::task_local;

#[cfg(feature = "macros")]
pub use tokio_orig::{join, main, try_join};

#[cfg(feature = "fs")]
pub use tokio_orig::fs;

#[cfg(feature = "signal")]
pub use tokio_orig::signal;
