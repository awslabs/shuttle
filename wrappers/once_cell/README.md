# Shuttle support for `once_cell`

This crate contains the wrapper that enables testing of applications that use [once_cell](https://crates.io/crates/once_cell) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "once_cell/shuttle",
]

[dependencies]
once_cell = { package = "shuttle-once_cell", version = "VERSION_NUMBER" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided. Under the flag, `once_cell::sync::{OnceCell, Lazy}` and `once_cell::race::{OnceNonZeroUsize, OnceBool, OnceRef, OnceBox}` are Shuttle's models: a `static` cell is empty again in every execution, initializing a cell is where tasks interleave, and a failing schedule replays. `once_cell::unsync` is once_cell's own.

## Limitations

For the list of current limitations, see the [shuttle-once_cell-inner](https://crates.io/crates/shuttle-once_cell-inner) inner crate.
