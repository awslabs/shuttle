# Shuttle support for `lazy_static`

This crate contains the wrapper that enables testing of applications that use [lazy_static](https://crates.io/crates/lazy_static) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "lazy_static/shuttle",
]

[dependencies]
lazy_static = { package = "shuttle-lazy_static", version = "VERSION_NUMBER" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided.

## Limitations

For the list of current limitations, see the [shuttle-lazy_static-impl](https://crates.io/crates/shuttle-lazy_static-impl) inner crate.
