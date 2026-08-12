# Shuttle support for `tokio-util`

This crate contains the implementation that enables testing of applications that use [tokio-util](https://crates.io/crates/tokio-util) with Shuttle. It should not be depended on directly, depend on `shuttle-tokio-util` instead.

This crate is a fork of `tokio-util` 0.7.11, with the `tokio` dependency replaced by Shuttle's tokio implementation so that Shuttle's scheduler can control and observe the concurrency within it.

## Limitations

The implemented surface covers the `sync` module, the `codec` module (under the `codec` feature), and the `task` module (under the `rt` feature). Other parts of `tokio-util`, such as `io` and `compat`, are not yet provided; the corresponding Cargo features are accepted for compatibility with upstream's feature set but currently expose no Shuttle implementation. If your project needs functionality which is not currently supported, please file an issue or, better yet, open a PR to contribute the functionality.
