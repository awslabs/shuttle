//! [`io-uring`](https://crates.io/crates/io-uring) wrapper for the
//! [Shuttle](https://crates.io/crates/shuttle) concurrency testing tool.
//!
//! Depend on it in place of `io-uring`:
//!
//! ```toml
//! [features]
//! shuttle = ["io-uring/shuttle"]
//!
//! [dependencies]
//! io-uring = { package = "shuttle-io-uring", version = "0.7" }
//! ```
//!
//! Without the `shuttle` feature this re-exports `io-uring`, the kernel ring.
//! With it, it re-exports `shuttle-io-uring-impl`: a simulated kernel whose
//! completions Shuttle schedules, so code built on the ring runs unchanged
//! under Shuttle and its failing schedules replay. That crate also provides
//! the `AsyncFd` a reactor waits on for the ring's readiness, which tokio's
//! I/O driver cannot provide under Shuttle.

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_io_uring_impl::*;
    } else {
        pub use io_uring::*;
    }
}
