//! Shuttle's implementation of an async executor, roughly equivalent to [`futures::executor`].
//!
//! The [spawn] method spawns a new asynchronous task that the executor will run to completion. The
//! [block_on] method blocks the current thread on the completion of a future.
//!
//! [`futures::executor`]: https://docs.rs/futures/0.3.13/futures/executor/index.html

pub use shuttle_std::future::*;
