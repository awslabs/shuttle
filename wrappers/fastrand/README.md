# Shuttle support for `fastrand`

This crate contains the wrapper that enables testing of applications that use [fastrand](https://crates.io/crates/fastrand) 2 with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "fastrand/shuttle",
]

[dependencies]
fastrand = { package = "shuttle-fastrand", version = "2" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will draw its randomness from Shuttle when the `shuttle` feature flag is provided, so that a failing test replays.

## Limitations

For the list of current limitations, see the [inner crate](https://crates.io/crates/shuttle-fastrand-inner).
