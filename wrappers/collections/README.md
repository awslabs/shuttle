# Determinizable collections

This crate provides `HashMap` and `HashSet` types that can be switched between the standard library implementations and deterministic implementations via a feature flag.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "determinizable-collections/deterministic",
]

[dependencies]
determinizable-collections = "VERSION_NUMBER"
```

Without the `deterministic` feature, the standard library `HashMap` and `HashSet` are re-exported. With the `deterministic` feature enabled, deterministic versions are provided instead, where iteration order is reproducible across runs.

This is useful for [Shuttle](https://crates.io/crates/shuttle) testing, where deterministic iteration order is required to replay test failures. It can also be enabled for all tests or pre-production environments to make failures easier to reproduce.

## Limitations

For details on the deterministic implementation, see the README in [deterministic_collections](deterministic_collections).
