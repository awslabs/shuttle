# Shuttle support for `tokio`

This crate contains the Shuttle-compatible replacements for the tokio primitives that Shuttle needs to model, and is an internal dependency of [shuttle-tokio-impl](https://crates.io/crates/shuttle-tokio-impl). It should not be depended on directly, depend on `shuttle-tokio` instead.

The types here (the runtime entry points, the `sync` primitives, `time`, and the task APIs) are backed by Shuttle's own primitives so that Shuttle's scheduler can control and observe every operation. `shuttle-tokio-impl` combines them with re-exports of the real `tokio` crate to present tokio's full module layout.

## Limitations

Shuttle's tokio support does not currently model all tokio functionality. Some parts of tokio have not been implemented or may not be modeled faithfully. Keep this in mind when using Shuttle with tokio, as you may encounter missing functionality that needs to be added. If you encounter missing features, please file an issue or, better yet, open a PR to contribute the functionality.

The list of constructs not supported by Shuttle are in [Issue 241](https://github.com/awslabs/shuttle/issues/241).
