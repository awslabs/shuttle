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

## What is modelled

`Mutex`, `FairMutex`, `ReentrantMutex`, `RwLock` (with their guards, including the mapped and
`Arc`-based ones), `Condvar`, `Once`, the raw locks and the `const` constructors: every stable
`parking_lot` sync type.

## Limitations

For the list of current limitations, see the README in [parking_lot_impl](parking_lot_impl).