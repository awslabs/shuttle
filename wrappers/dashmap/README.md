# Shuttle support for `dashmap`

This crate contains the wrapper that enables testing of applications that use [dashmap](https://crates.io/crates/dashmap) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "dashmap/shuttle",
]

[dependencies]
dashmap = { package = "shuttle-dashmap", version = "VERSION_NUMBER" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided.

## Limitations

For the list of current limitations, see the README in [shuttle-dashmap_impl](https://crates.io/crates/shuttle-dashmap_impl).
