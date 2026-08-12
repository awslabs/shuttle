# Shuttle support for `async-stream`

This crate contains the implementation that enables testing of applications that use [async-stream](https://crates.io/crates/async-stream) with Shuttle. It should not be depended on directly, depend on `shuttle-async-stream` instead.

## Limitations

There should be no limitations compared to [async-stream](https://crates.io/crates/async-stream). This crate is a fork of the 0.3.6 version, where the only change from the original is that the thread-local in `yielder.rs` has been made Shuttle-compatible.
