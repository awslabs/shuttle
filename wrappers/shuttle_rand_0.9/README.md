# Shuttle support for `rand` 0.9

This crate contains the wrapper that enables testing of applications that use [rand](https://crates.io/crates/rand) 0.9 with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "rand/shuttle",
]

[dependencies]
rand = { package = "shuttle-rand_0_9", version = "0.9" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will draw its randomness from Shuttle when the `shuttle` feature flag is provided, so that a failing test replays.

The crate is named `shuttle-rand_0_9` rather than `shuttle-rand` because `shuttle-rand` is the wrapper for rand 0.8, and a Cargo workspace can hold only one package of a name.

## Limitations

For the list of current limitations, see the [inner crate](https://crates.io/crates/shuttle-rand_0_9-inner).
