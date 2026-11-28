# Shuttle support for `crossbeam-skiplist`

This crate contains the wrapper that enables testing of applications that use
[`crossbeam-skiplist`](https://crates.io/crates/crossbeam-skiplist) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "crossbeam-skiplist/shuttle",
]

[dependencies]
crossbeam-skiplist = { package = "shuttle-crossbeam-skiplist", version = "0.1" }
```

The code will behave as before when the `shuttle` feature flag is not provided, and will use
Shuttle-compatible maps and sets with crossbeam-skiplist's API when the `shuttle` feature flag is
provided.

## What the `shuttle` feature changes

- `SkipMap` and `SkipSet`, with their entries and iterators, are Shuttle's models: each operation
  happens at a scheduling point on a sorted sequence of reference-counted entries. Entries stay
  readable after removal, iterators are not snapshots, and replacing an entry takes two steps, as
  in crossbeam.
- A map's entries belong to the running execution, so a map the test closure captured starts every
  execution as it was built.
- The operations panic outside of a Shuttle test, as the rest of Shuttle does.
- crossbeam-skiplist's guard-based `base` module and `SkipList` are not modelled.

See [the crate documentation](https://docs.rs/shuttle-crossbeam-skiplist-inner) for the details, and
for what the model does not reproduce exactly.
