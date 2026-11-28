# Shuttle support for `crossbeam-epoch`

This crate contains the wrapper that enables testing of applications that use
[`crossbeam-epoch`](https://crates.io/crates/crossbeam-epoch) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "crossbeam-epoch/shuttle",
]

[dependencies]
crossbeam-epoch = { package = "shuttle-crossbeam-epoch", version = "0.9" }
```

The code will behave as before when the `shuttle` feature flag is not provided, and will use
Shuttle-controlled atomic pointers with crossbeam-epoch's API when the `shuttle` feature flag is
provided.

## What the `shuttle` feature changes

- Every operation on an `Atomic` is one scheduling point, so Shuttle explores the orders in which
  concurrent operations take effect. Pinning is modelled, reentrant, and takes the scheduling points
  crossbeam's does. `Pointable`, `Pointer` and `CompareAndSetOrdering` stay crossbeam-epoch's own
  traits.
- **Nothing is reclaimed**: a deferred function is never called, so an object whose destruction is
  deferred stays allocated for the rest of the process. There is therefore no use-after-free under
  Shuttle, and no bug that needs reclamation to show up can be caught. A function deferred on an
  `unprotected` guard still runs immediately, as in crossbeam.
- An `Atomic`'s word belongs to the running execution and starts every execution at its construction
  value, so a `static Atomic` is as constructed at the start of each one.
- Operations on an `Atomic`, `pin` and `is_pinned` panic outside of a Shuttle test, as the rest of
  Shuttle does.

See [the crate documentation](https://docs.rs/shuttle-crossbeam-epoch-inner) for the scheduling
points, the leak model and the rest of the differences.
