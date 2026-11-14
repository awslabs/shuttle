# Shuttle support for `crossbeam-utils`

This crate contains the wrapper that enables testing of applications that use [crossbeam-utils](https://crates.io/crates/crossbeam-utils) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "crossbeam-utils/shuttle",
]

[dependencies]
crossbeam-utils = { package = "shuttle-crossbeam-utils", version = "0.8" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided.

## What changes under the feature

Every item keeps crossbeam-utils 0.8's API:

- `Backoff::spin` and `snooze` take one scheduling point each, so a busy-wait lets the task it waits for run.
- `atomic::AtomicCell`'s operations are atomic steps of the execution, whatever `T` is, with the causality of one of Shuttle's atomics. `atomic::AtomicConsume` is implemented for Shuttle's atomics as well as `std`'s.
- `sync::ShardedLock` is Shuttle's `RwLock`, `sync::WaitGroup` uses Shuttle's `Mutex` and `Condvar`, and `sync::Parker` blocks a Shuttle task, so Shuttle explores the orders in which the threads reach them and reports a deadlock instead of hanging.
- `thread::scope`'s threads are Shuttle tasks, on `shuttle::thread::scope`.
- **`Parker::park_timeout` times out only when no task can run**, and then moves `shuttle-time`'s clock to its deadline, so a park never returns early in a schedule where the unparking task could still have run. `park_deadline` takes `shuttle_time::time::Instant`.
- `CachePadded` is crossbeam-utils's own type in both modes: it aligns a value and has nothing to schedule.

## Limitations

For the details and the current limitations, see the [shuttle-crossbeam-utils-inner](https://crates.io/crates/shuttle-crossbeam-utils-inner) inner crate.
