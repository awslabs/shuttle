# Shuttle support for `io-uring`

This crate contains the wrapper that enables testing of applications that use [io-uring](https://crates.io/crates/io-uring) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "io-uring/shuttle",
]

[dependencies]
io-uring = { package = "shuttle-io-uring", version = "VERSION_NUMBER" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run against a simulated kernel ring whose completions Shuttle schedules when the `shuttle` feature flag is provided.

## Limitations

For the model and its current limitations, see the README in [io_uring_impl](io_uring_impl).
