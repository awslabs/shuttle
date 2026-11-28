# Shuttle support for `std::time`

This crate contains the wrapper that enables testing of applications that read `std::time`'s clocks
with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "shuttle-time/shuttle",
]

[dependencies]
shuttle-time = "VERSION_NUMBER"
```

Then swap usages of `std::time` with `shuttle_time::time`. The code will behave as before when the
`shuttle` feature flag is not provided, and will read a clock Shuttle controls when the `shuttle`
feature flag is provided.

## What the `shuttle` feature changes

Without the feature, `shuttle_time::time` is `std::time`. With it, `Instant` and `SystemTime` are
this crate's own types with `std::time`'s API, and their `now()` reads a counter of nanoseconds that
belongs to the running execution:

- Both types read the same counter, so the two clocks agree with each other.
- Where the scheduler provides random data, as the random and PCT schedulers do, each call
  advances the counter by zero or one nanosecond, chosen by a random bool drawn from Shuttle, which
  records the draw in the schedule. Where it does not, as under `shuttle::check_dfs`, each call
  advances it by exactly one nanosecond, without a draw, so reading the clock adds no choice to the
  search. Either way a failing schedule replays, and `check_uncontrolled_nondeterminism` does not
  report the clock.
- The counter starts over in every execution. The first `SystemTime::now()` of an execution is
  2026-01-01T00:00:00Z.
- Reading the clock is not a scheduling point, as reading the operating system's clock is not one
  either.

**The timestamps track nothing real.** They say only how many times the execution has read the clock
and in which order; nothing here measures how long anything took.

This is not virtual time: the crate models no timers, and `std`'s and tokio's sleeps and timeouts
do not advance the counter. A timer of another Shuttle wrapper (`shuttle-crossbeam-channel`'s `after`,
`tick` and timeouts) moves the counter forward to its deadline when it fires.

`Duration`, `SystemTimeError` and `TryFromFloatSecsError` are `std::time`'s in both modes, and so
`UNIX_EPOCH` still means 1970-01-01T00:00:00Z.

## Limitations

- Outside of a Shuttle test, `now()` panics, as the rest of Shuttle does.
- `SystemTime` converts to and from `std::time::SystemTime` with `From`. `Instant` does not:
  `std::time::Instant` is opaque, so neither direction can be written.

See [the crate documentation](https://docs.rs/shuttle-time-inner) for the details.
