//! This crate contains Shuttle's internal implementation of the `async-stream` crate.
//! Do not depend on this crate directly. Use the `shuttle-async-stream` crate, which conditionally
//! exposes this implementation with the `shuttle` feature or the original crate without it.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//!
//! [`async-stream`]: <https://crates.io/crates/async-stream>

#![warn(missing_debug_implementations, rust_2018_idioms, unreachable_pub)]
#![doc(test(no_crate_inject, attr(deny(rust_2018_idioms))))]

// SHUTTLE_CHANGES:
// The only change that has been made with regards to `async-stream` is that the `thread-local` in `yielder.rs` has been made Shuttle-compatible,
// and the [ui](https://github.com/tokio-rs/async-stream/tree/master/async-stream/tests/ui) tests have been removed, as they relied on `tokio::main`.

mod async_stream;
mod next;
mod yielder;

/// Asynchronous stream
#[macro_export]
macro_rules! stream {
    ($($tt:tt)*) => {
        $crate::__private::stream_inner!(($crate) $($tt)*)
    }
}

/// Asynchronous fallible stream
#[macro_export]
macro_rules! try_stream {
    ($($tt:tt)*) => {
        $crate::__private::try_stream_inner!(($crate) $($tt)*)
    }
}

// Not public API.
#[doc(hidden)]
pub mod __private {
    pub use crate::async_stream::AsyncStream;
    pub use crate::next::next;
    pub use async_stream_impl::{stream_inner, try_stream_inner};
    pub mod yielder {
        pub use crate::yielder::pair;
    }
}
