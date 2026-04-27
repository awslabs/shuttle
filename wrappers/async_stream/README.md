# Shuttle support for `async-stream`

This folder contains the implementation and wrapper that enables testing of [async-stream](https://crates.io/crates/async-stream) applications with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "async-stream/shuttle",
]

[dependencies]
async-stream = { package = "shuttle-async-stream", version = "VERSION_NUMBER" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided.

## Limitations

There should be no limitations compared to [async-stream](https://crates.io/crates/async-stream). The version here is a fork of the 0.3.6 version, where the only change from the original is that the thread-local in `yielder.rs` has been made Shuttle-compatible.
