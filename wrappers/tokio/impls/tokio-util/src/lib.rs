#![allow(unknown_lints, unexpected_cfgs)]
//! A fork of [https://docs.rs/crate/tokio-util](https://docs.rs/crate/tokio-util)
//! This package is not intended to be depended on directly, and is not currently published.
//! It is a fork of the tokio-util package with imports swapped so that it builds atop Shuttle.
//! It exists for the other packages in the shuttle-tokio-"family" to use.

#[macro_use]
mod cfg;

cfg_rt! {
    pub mod task;
}

cfg_codec! {
    pub mod codec;
}

pub mod sync;

mod util;
