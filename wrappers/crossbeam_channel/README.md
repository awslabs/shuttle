# Shuttle support for `crossbeam-channel`

This crate contains the wrapper that enables testing of applications that use [crossbeam-channel](https://crates.io/crates/crossbeam-channel) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "crossbeam-channel/shuttle",
]

[dependencies]
crossbeam-channel = { package = "shuttle-crossbeam-channel", version = "0.5" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible channels when the `shuttle` feature flag is provided.

## What changes under the feature

Every item keeps crossbeam-channel 0.5's API, but the channels are Shuttle's:

- Sends, receives and selections are scheduling points, and a blocked one is a blocked Shuttle task, so Shuttle explores the orders in which the threads use the channels.
- `select!` and `Select` pick among the ready operations with Shuttle's random number generator, so the same seed gives the same choices and a failing schedule replays.
- **A timeout fires only when no task can run**, and then moves `shuttle-time`'s clock to its deadline. The timers of `after`, `at` and `tick` work the same way. A timeout never fires in a schedule where its operation could still have completed, and a program that relies on a timeout to make progress is not reported as deadlocked.
- A channel's `Instant` is `shuttle-time`'s, so code that passes deadlines takes `Instant` from `shuttle_time::time`. The `shuttle` feature turns on `shuttle-time`'s `shuttle` feature too.
- The error types are crossbeam-channel's own in both modes.

## Limitations

For the details and the current limitations, see the [shuttle-crossbeam-channel-inner](https://crates.io/crates/shuttle-crossbeam-channel-inner) inner crate.
