# Shuttle support for `crossbeam-deque`

This crate contains the wrapper that enables testing of applications that use
[`crossbeam-deque`](https://crates.io/crates/crossbeam-deque) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "crossbeam-deque/shuttle",
]

[dependencies]
crossbeam-deque = { package = "shuttle-crossbeam-deque", version = "0.8" }
```

The code will behave as before when the `shuttle` feature flag is not provided, and will use
Shuttle-compatible work-stealing queues with crossbeam-deque's API when the `shuttle` feature flag is
provided.

## What the `shuttle` feature changes

- `Worker`, `Stealer` and `Injector` are Shuttle's models; `Steal` is crossbeam-deque's own type.
- Pushes, pops, `len` and `is_empty` happen in one step at a scheduling point. A steal reads the
  queue at one scheduling point and takes its task at a second, and returns `Steal::Retry` exactly
  when crossbeam's would: when another operation moved the front of the queue in between, or a
  worker swapped its buffer. The schedule decides which steals retry, so Shuttle explores them and a
  failing schedule replays.
- A queue's tasks belong to the running execution, so a queue the test closure captured starts every
  execution empty.
- The operations panic outside of a Shuttle test, as the rest of Shuttle does.

See [the crate documentation](https://docs.rs/shuttle-crossbeam-deque-inner) for the details.
