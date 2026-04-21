# Shuttle support for `std::sync`

This crate contains the wrapper that enables testing of applications that use `std::sync` with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "shuttle-sync/shuttle",
]

[dependencies]
shuttle-sync = "VERSION_NUMBER"
```

Then swap usages of `std::sync` with `shuttle_sync::sync`. The code will behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided.
