#![warn(missing_debug_implementations, missing_docs, rust_2018_idioms, unreachable_pub)]
#![doc(test(
    no_crate_inject,
    attr(deny(warnings, rust_2018_idioms), allow(dead_code, unused_variables))
))]

//! A fork of [https://docs.rs/crate/tokio-test](https://docs.rs/crate/tokio-test)
//! This package is not intended to be depended on directly, and is not currently published.
//! It is a fork of the tokio-util package with imports swapped so that it builds atop Shuttle.
//! It exists for the other packages in the shuttle-tokio-"family" to use.

pub mod io;
pub mod stream_mock;

mod macros;
pub mod task;

/// Runs the provided future, blocking the current thread until the
/// future completes.
///
/// For more information, see the documentation for
/// [`tokio::runtime::Runtime::block_on`][runtime-block-on].
///
/// [runtime-block-on]: https://docs.rs/tokio/1.3.0/tokio/runtime/struct.Runtime.html#method.block_on
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use tokio::runtime;

    let rt = runtime::Builder::new_current_thread().enable_all().build().unwrap();

    rt.block_on(future)
}
