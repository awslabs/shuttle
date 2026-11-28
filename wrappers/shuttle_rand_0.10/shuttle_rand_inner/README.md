# Shuttle support for `rand` 0.10

This crate contains the implementation that enables testing of applications that use [rand](https://crates.io/crates/rand) 0.10 with Shuttle. It should not be depended on directly, depend on `shuttle-rand_0_10` instead.

## What it changes

Every way rand 0.10 has of reaching entropy draws from Shuttle's random number generator, which records each value in the schedule, so that a failing execution replays: `rng()`, `random()`, `random_iter()`, `random_range()`, `random_bool()`, `random_ratio()`, `fill()`, `make_rng()`, `rngs::ThreadRng` and `rngs::SysRng`. Everything else is rand's own, including `SeedableRng` and the seeded generators: `StdRng::seed_from_u64(7)` produces exactly what it produces with rand.

## Limitations

- Outside of a Shuttle test, the entry points above panic, as the rest of Shuttle does.
- `shuttle::check_dfs` refuses random data, so a test that uses the entry points above panics under it with "requested random data from DFS scheduler". A generator seeded with a fixed seed works.
- Drawing a value is not a scheduling point.
- `getrandom` reached directly (`getrandom::SysRng`, `getrandom::fill`) reads the operating system.
- `ThreadRng::reseed` does nothing, since Shuttle's generator cannot be reseeded.
