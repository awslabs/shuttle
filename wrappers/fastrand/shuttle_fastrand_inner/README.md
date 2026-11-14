# Shuttle support for `fastrand`

This crate contains the implementation that enables testing of applications that use [fastrand](https://crates.io/crates/fastrand) 2 with Shuttle. It should not be depended on directly, depend on `shuttle-fastrand` instead.

## What it changes

fastrand's free functions (`fastrand::u64(..)`, `fastrand::shuffle`, `fastrand::alphanumeric` and the rest) use a generator per thread, seeded from the clock. Here they use a generator per Shuttle task, and per execution, which the task's first use seeds from Shuttle's random number generator, so that a failing execution replays. `Rng::new` forks that generator, as fastrand's forks the thread's. `seed` and `get_seed` act on it as they do in fastrand, and `Rng::with_seed` is fastrand's own: a seeded generator produces exactly what it produces with fastrand.

## Limitations

- Outside of a Shuttle test, the free functions and `Rng::new` panic, as the rest of Shuttle does.
- `shuttle::check_dfs` refuses random data, so a test that uses the free functions panics under it with "requested random data from DFS scheduler", unless each task calls `fastrand::seed` first.
- Drawing a value is not a scheduling point.
- `Rng` is this crate's own type, with fastrand's API. It converts to and from fastrand's `Rng` with `From`.
