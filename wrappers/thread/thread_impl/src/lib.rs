//! This crate contains Shuttle's implementation of [`std::thread`], [`std::thread_local!`] and
//! [`std::hint`]. Do not depend on this crate directly. Use the `shuttle-thread` crate, which
//! conditionally exposes this implementation with the `shuttle` feature or the originals without it.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>

/// Shuttle's [`std::thread`].
pub mod thread {
    pub use shuttle::thread::*;
}

/// Shuttle's [`std::hint`], whose `spin_loop` takes a scheduling point.
pub mod hint {
    pub use shuttle::hint::*;
}

pub use shuttle::thread_local;
