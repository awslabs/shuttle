# Shuttle support for `tokio-util`

This crate contains the wrapper that enables testing of applications that use [tokio-util](https://crates.io/crates/tokio-util) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "tokio-util/shuttle",
]

[dependencies]
tokio-util = { package = "shuttle-tokio-util", version = "VERSION_NUMBER" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided.

## Limitations

For the list of current limitations, see the [shuttle-tokio-util-impl](https://crates.io/crates/shuttle-tokio-util-impl) inner crate.
