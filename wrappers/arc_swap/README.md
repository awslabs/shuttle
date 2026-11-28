# Shuttle support for `arc-swap`

This crate contains the wrapper that enables testing of applications that use
[arc-swap](https://crates.io/crates/arc-swap) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "arc-swap/shuttle",
]

[dependencies]
arc-swap = { package = "shuttle-arc-swap", version = "VERSION_NUMBER" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided.

Under Shuttle every `load`, `store`, `swap`, `compare_and_swap` and `rcu` is one scheduling point and one step, so the operations are linearizable and Shuttle explores the interleavings around them; a loaded `Guard` holds a clone of the smart pointer, so a value is never freed under its reader; and the stored value lives in the running execution, so a `static` holds the value it was constructed with at the start of every execution.

## Limitations

For the list of current limitations, see the README in [shuttle_arc_swap_inner](shuttle_arc_swap_inner).
