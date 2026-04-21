# Shuttle support for `rand`

This crate contains the wrapper that enables testing of applications that use [rand](https://crates.io/crates/rand) 0.8 with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "rand/shuttle",
]

[dependencies]
rand = { package = "shuttle-rand", version = "VERSION_NUMBER" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided.

## Limitations

For the list of current limitations, see the [rand](https://crates.io/crates/shuttle-rand_0_8-inner) inner crate.
