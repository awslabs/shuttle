# Shuttle support for `tokio-retry`

This crate contains the implementation that enables testing of applications that use [tokio-retry](https://crates.io/crates/tokio-retry) with Shuttle. It should not be depended on directly, depend on `shuttle-tokio-retry` instead.

## Limitations

The implementation replaces `rand` and `tokio` with their Shuttle-compatible equivalents. The retry strategies and API surface match upstream `tokio-retry` 0.3.
