# Shuttle support for `rand`

This crate contains the implementation that enables testing of applications that use [rand](https://crates.io/crates/rand) 0.8 with Shuttle. It should not be depended on directly, depend on `shuttle-rand` instead.

## Limitations

`StdRng` and `SmallRng` are currently implemented in terms of Shuttle's `ThreadRng`, meaning they do not actually use the provided seed. `SeedableRng::from_seed` is accepted but ignored. `PartialEq` and `Eq` on these types are stubbed and will panic if called.
