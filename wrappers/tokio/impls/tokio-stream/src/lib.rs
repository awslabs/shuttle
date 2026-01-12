#![allow(
    clippy::cognitive_complexity,
    clippy::large_enum_variant,
    clippy::needless_doctest_main
)]
#![warn(missing_debug_implementations, missing_docs, rust_2018_idioms, unreachable_pub)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc(test(
    no_crate_inject,
    attr(deny(warnings, rust_2018_idioms), allow(dead_code, unused_variables))
))]

//! This crate contains [`Shuttle`] internal implementations of the [`tokio-stream`] crate.
//! Do not depend on this crate directly. Use the `shuttle-tokio-stream` crate instead, which
//! exposes these Shuttle-compatible implementations when the `shuttle` feature is enabled,
//! or the original tokio-stream functionality when the feature is disabled.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//!
//! [`tokio-stream`]: <https://crates.io/crates/tokio-stream>

#[macro_use]
mod macros;

pub mod wrappers;

mod stream_ext;
pub use stream_ext::{collect::FromStream, StreamExt};
cfg_time! {
    pub use stream_ext::timeout::{Elapsed, Timeout};
}

mod empty;
pub use empty::{empty, Empty};

mod iter;
pub use iter::{iter, Iter};

mod once;
pub use once::{once, Once};

mod pending;
pub use pending::{pending, Pending};

mod stream_map;
pub use stream_map::StreamMap;

mod stream_close;
pub use stream_close::StreamNotifyClose;

#[doc(no_inline)]
pub use futures_core::Stream;
