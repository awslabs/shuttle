# Shuttle support for `tokio-retry`

This crate contains the wrapper that enables testing of applications that use [tokio-retry](https://crates.io/crates/tokio-retry) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "tokio-retry/shuttle",
]

[dependencies]
tokio-retry = { package = "shuttle-tokio-retry", version = "VERSION_NUMBER" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided.

## Limitations

For the list of current limitations, see the [tokio-retry](https://crates.io/crates/shuttle-tokio-retry-impl) inner crate.
