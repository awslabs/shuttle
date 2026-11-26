# Shuttle support for `spin`

This crate contains the wrapper that enables testing of applications that use
[spin](https://crates.io/crates/spin) 0.10 with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "spin/shuttle",
]

[dependencies]
spin = { package = "shuttle-spin", version = "VERSION_NUMBER" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided.

Under Shuttle a task that waits for one of spin's locks blocks in Shuttle's scheduler instead of spinning, so the task that holds the lock gets to run; every operation on a lock is a scheduling point; and the state of a `static` resets at the start of each execution.

## Limitations

For the list of current limitations, see the README in [shuttle_spin_inner](shuttle_spin_inner).
