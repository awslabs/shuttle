# Shuttle support for `tokio`

This crate contains the implementation that enables testing of applications that use [tokio](https://crates.io/crates/tokio) with Shuttle. It should not be depended on directly, depend on `shuttle-tokio` instead.

## Implementation

This crate re-exports the parts of tokio that Shuttle does not need to model (from the real `tokio` crate) alongside the Shuttle-compatible replacements for the parts it does (from the [shuttle-tokio-impl-inner](https://crates.io/crates/shuttle-tokio-impl-inner) crate), so that the combined surface matches `tokio`'s module layout.

## Limitations

Shuttle's tokio support does not currently model all tokio functionality. Some parts of tokio have not been implemented or may not be modeled faithfully. Keep this in mind when using Shuttle with tokio, as you may encounter missing functionality that needs to be added. If you encounter missing features, please file an issue or, better yet, open a PR to contribute the functionality.

The list of constructs not supported by Shuttle are in [Issue 241](https://github.com/awslabs/shuttle/issues/241).
