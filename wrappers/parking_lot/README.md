# Shuttle support for `parking_lot`

This crate contains the wrapper that enables testing of applications that use [parking_lot](https://crates.io/crates/parking_lot) with Shuttle.

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

For the list of current limitations, see the README in [parking_lot_impl](parking_lot_impl).