# Shuttle support for `crossbeam-queue`

This crate contains the wrapper that enables testing of applications that use
[`crossbeam-queue`](https://crates.io/crates/crossbeam-queue) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "crossbeam-queue/shuttle",
]

[dependencies]
crossbeam-queue = { package = "shuttle-crossbeam-queue", version = "0.3" }
```

The code will behave as before when the `shuttle` feature flag is not provided, and will use
Shuttle-compatible queues with crossbeam-queue's API when the `shuttle` feature flag is provided.

## What the `shuttle` feature changes

- `ArrayQueue` and `SegQueue` are FIFO queues whose operations happen in one step at a scheduling
  point (`push`, `force_push`, `pop`, `len`, `is_empty`, `is_full`), so they are linearizable and
  Shuttle explores every order in which concurrent operations take effect.
- A queue's contents belong to the running execution: a `static` `SegQueue`, or a queue the test
  closure captured, starts every execution empty, so that a failing schedule replays.
- The operations panic outside of a Shuttle test, as the rest of Shuttle does.

See [the crate documentation](https://docs.rs/shuttle-crossbeam-queue-inner) for the details.
