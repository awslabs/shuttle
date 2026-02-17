# Shuttle support for `parking_lot`

This folder contains the implementation and wrapper that enables testing of [parking_lot](https://crates.io/crates/parking_lot) applications with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "parking_lot/shuttle",
]

[dependencies]
parking_lot = { package = "shuttle-parking_lot", version = "VERSION_NUMBER" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided.

## Limitations

Shuttle's parking_lot functionality is currently limited to a subset of the `Mutex` and `RwLock` primitives. If your project needs functionality which is not currently supported, please file an issue or, better yet, open a PR to contribute the functionality.