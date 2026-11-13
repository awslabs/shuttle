# Shuttle support for `std::thread`

This crate contains the wrapper that enables testing of applications that use `std::thread`,
`thread_local!` and `std::hint::spin_loop` with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "shuttle-thread/shuttle",
]

[dependencies]
shuttle-thread = "VERSION_NUMBER"
```

Then swap usages of `std::thread` with `shuttle_thread::thread`, of `std::hint` with
`shuttle_thread::hint`, and of `std::thread_local!` with `shuttle_thread::thread_local!`. The code
will behave as before when the `shuttle` feature flag is not provided, and will run with
Shuttle-compatible primitives when the `shuttle` feature flag is provided.

## What changes under the feature

A thread becomes a Shuttle task, which the scheduler runs on the one OS thread an execution uses,
so Shuttle explores the interleavings of the threads instead of leaving them to the OS. `sleep`
takes a single scheduling point rather than sleeping, `park`/`unpark` and `yield_now` go through
the scheduler, `spin_loop` yields, `available_parallelism` reports the configured value (4 by
default) instead of the host's processor count, and a `thread_local!` value belongs to a task and
is dropped when the execution ends. The crate documentation lists each of these.

## Limitations

- `std::thread::JoinHandleExt` (the Unix `as_pthread_t` extension) has no Shuttle equivalent, as a
  Shuttle task has no OS thread of its own.
- `thread::sleep` and `park_timeout` do not model time. A test cannot use them to wait for
  something to happen, and an execution in which every task is parked is reported as a deadlock.
- `JoinHandle::is_finished` takes no scheduling point, so a loop around it needs a `yield_now` or a
  `spin_loop` to let the other task finish.
