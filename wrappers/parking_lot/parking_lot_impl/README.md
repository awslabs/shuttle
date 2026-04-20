# Shuttle support for `parking_lot`

This crate contains the implementation that enables testing of [parking_lot](https://crates.io/crates/parking_lot) applications with Shuttle. It should not be depended on directly, depend on `shuttle-parking_lot` instead.

## Limitations
Shuttle's parking_lot functionality is currently limited to a subset of the `Mutex` and `RwLock` primitives. If your project needs functionality which is not currently supported, please file an issue or, better yet, open a PR to contribute the functionality.