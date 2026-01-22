# Shuttle support for `tokio`

This folder contains the implementation and wrapper that enables testing of [tokio](https://crates.io/crates/tokio) applications with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "tokio/shuttle",
]

[dependencies]
tokio = { package = "shuttle-tokio", version = "VERSION_NUMBER" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided.

## Limitations

Shuttle's tokio support does not currently model all tokio functionality. Some parts of tokio have not been implemented or may not be modeled faithfully. Keep this in mind when using Shuttle with tokio, as you may encounter missing functionality that needs to be added. If you encounter missing features, please file an issue or, better yet, open a PR to contribute the functionality.

The list of constructs not supported by Shuttle are in [Issue 241](https://github.com/awslabs/shuttle/issues/241)
