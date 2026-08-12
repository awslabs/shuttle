# Shuttle support for `tokio-macros`

This crate contains the procedural macros that enable testing of applications that use [tokio](https://crates.io/crates/tokio) with Shuttle. It should not be depended on directly, depend on `shuttle-tokio` instead.

This crate is a fork of `tokio-macros` 2.2.0. It provides the `#[tokio::main]` and `#[tokio::test]` attribute macros, expanded to drive the Shuttle-compatible runtime rather than the real tokio runtime.
