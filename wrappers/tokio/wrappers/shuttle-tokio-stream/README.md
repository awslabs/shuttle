# Shuttle support for `tokio-stream`

This crate contains the wrapper that enables testing of applications that use [tokio-stream](https://crates.io/crates/tokio-stream) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "tokio-stream/shuttle",
]

[dependencies]
tokio-stream = { package = "shuttle-tokio-stream", version = "VERSION_NUMBER" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided.

## Limitations

For the list of current limitations, see the [shuttle-tokio-stream-impl](https://crates.io/crates/shuttle-tokio-stream-impl) inner crate.
