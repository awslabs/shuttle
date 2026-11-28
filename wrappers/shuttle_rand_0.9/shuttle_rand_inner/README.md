# Shuttle support for `rand` 0.9

This crate contains the implementation that enables testing of applications that use [rand](https://crates.io/crates/rand) 0.9 with Shuttle. It should not be depended on directly, depend on `shuttle-rand_0_9` instead.

## What it changes

Every way rand 0.9 has of reaching entropy draws from Shuttle's random number generator, which records each value in the schedule, so that a failing execution replays: `rng()`, `thread_rng()`, `random()`, `random_iter()`, `random_range()`, `random_bool()`, `random_ratio()`, `fill()`, `rngs::ThreadRng`, `rngs::OsRng`, and `SeedableRng::from_os_rng`/`try_from_os_rng`. Everything else is rand's own, including the seeded generators: `StdRng::seed_from_u64(7)` produces exactly what it produces with rand.

`SeedableRng` is a trait of this crate's own, with rand_core's items, implemented for every type that implements rand_core's `SeedableRng` and forwarding to it, except that `from_os_rng` and `try_from_os_rng` seed from Shuttle.

## Limitations

- Outside of a Shuttle test, the entry points above panic, as the rest of Shuttle does.
- `shuttle::check_dfs` refuses random data, so a test that uses the entry points above panics under it with "requested random data from DFS scheduler". A generator seeded with a fixed seed works.
- Drawing a value is not a scheduling point.
- `rand::rand_core` is rand_core's own, so `rand::rand_core::OsRng` and the `from_os_rng` of `rand::rand_core::SeedableRng` read the operating system.
- A module that has both this crate's `SeedableRng` and rand_core's in scope gets "multiple applicable items" errors. A type that implements `rand::SeedableRng` itself implements this crate's trait, not rand_core's.
- `ThreadRng::reseed` does nothing, since Shuttle's generator cannot be reseeded.
