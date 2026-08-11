# Shuttle support for `tokio`

This crate contains the wrapper that enables testing of applications that use [tokio](https://crates.io/crates/tokio) with Shuttle.

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

For the list of current limitations, see the [shuttle-tokio-impl](https://crates.io/crates/shuttle-tokio-impl) inner crate.
