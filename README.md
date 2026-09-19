# Shuttle

[![crates.io](https://img.shields.io/crates/v/shuttle.svg)](https://crates.io/crates/shuttle)
[![docs.rs](https://docs.rs/shuttle/badge.svg)](https://docs.rs/shuttle)
[![Tests](https://github.com/awslabs/shuttle/actions/workflows/tests.yml/badge.svg)](https://github.com/awslabs/shuttle/actions/workflows/tests.yml)

Shuttle is a library for testing concurrent Rust code. It takes control of the scheduler, so that
thread interleavings are chosen by Shuttle rather than by the OS. That gives you two things a normal
concurrency test cannot: interleavings are explored systematically, under a policy you choose, and
any failure you find can be replayed deterministically.

Shuttle is inspired by [Loom](https://github.com/tokio-rs/loom), but defaults to randomized testing
rather than exhaustive testing. This is a soundness–scalability trade-off: randomized testing is not
sound (a passing Shuttle test does not prove the code is correct), but it scales to much larger test
cases than exhaustive exploration does. Empirically, randomized testing is successful at finding most
concurrency bugs, which tend not to be adversarial. Shuttle *can* also run exhaustively, through
[`check_dfs`](#choosing-a-scheduler), but it implements no partial order reduction, so exhaustive
runs stay tractable only for very small tests.

A core goal of Shuttle is to require minimal changes to the code under test: you should be able to
point Shuttle at existing code rather than restructure that code for testing. To that end, beyond
the core library, this repository houses a collection of drop-in [wrappers](/wrappers) for popular
crates, which swap out a dependency rather than asking you to rewrite the code that uses it. The
largest of these is [tokio support](#testing-tokio-code).

## Getting started

Consider this simple piece of concurrent code:

```rust
use std::sync::{Arc, Mutex};
use std::thread;

let lock = Arc::new(Mutex::new(0u64));
let lock2 = lock.clone();

thread::spawn(move || {
    *lock.lock().unwrap() = 1;
});

assert_eq!(0, *lock2.lock().unwrap());
```

There is an obvious race condition here: if the spawned thread runs before the assertion, the
assertion will fail. But writing a unit test that finds this execution is tricky. We could run
the test many times and try to "get lucky" by finding a failing execution, but that's not a very
reliable testing approach. Even if the test does fail, it will be difficult to debug: we won't
be able to easily catch the failure in a debugger, and every time we make a change, we will need
to run the test many times to decide whether we fixed the issue.

A Shuttle version of the test wraps the body in a call to `check_random`, and replaces the
concurrency-related imports from `std` with imports from `shuttle`:

```rust
use shuttle::sync::{Arc, Mutex};
use shuttle::thread;

shuttle::check_random(|| {
    let lock = Arc::new(Mutex::new(0u64));
    let lock2 = lock.clone();

    thread::spawn(move || {
        *lock.lock().unwrap() = 1;
    });

    assert_eq!(0, *lock2.lock().unwrap());
}, 100);
```

This runs the test body 100 times, each under a different randomized schedule, and detects the
assertion failure with extremely high probability (over 99.9999%).

The example above rewrites its imports because it is a self-contained snippet. Don't do that to a
real codebase. Depend on the [`shuttle-sync`](/wrappers/shuttle_sync) wrapper instead, and put it
behind a feature flag so that production builds are unaffected:

```toml
[features]
shuttle = ["shuttle-sync/shuttle"]

[dependencies]
shuttle-sync = "0.1"
```

Import from `shuttle_sync::sync` in place of `std::sync`, once, and then leave your code alone:

```rust
use shuttle_sync::sync::{Arc, Mutex};
```

Without the `shuttle` feature that import *is* `std::sync`. With it, the same import resolves to
Shuttle's instrumented primitives. Run your Shuttle tests with `cargo test --features shuttle`.

Two things to know. `shuttle-sync` re-exports all of `std::sync`, which is a superset of what
`shuttle::sync` models, so anything Shuttle does not implement (`OnceLock`, `LazyLock`, `mpmc`) has
to keep coming from `std` directly. And there is no wrapper for `std::thread` yet, so spawning is
still a feature switch at the import site:

```rust
#[cfg(feature = "shuttle")]
use shuttle::thread;
#[cfg(not(feature = "shuttle"))]
use std::thread;
```

The [wrappers README](/wrappers/README.md) covers the same dependency-swap pattern for `tokio`,
`parking_lot`, `dashmap`, `rand`, and the rest.

## Reproducing a failure

When a Shuttle test fails, it prints the schedule that caused the failure:

```text
test panicked in task "task-0" with schedule: "910102ccdedf9592aba2afd70104"
pass that schedule string into `shuttle::replay` to reproduce the failure
```

Passing that string to `shuttle::replay` reruns the test exactly once, along exactly that
interleaving:

```rust
shuttle::replay(|| {
    // ... same test body ...
}, "910102ccdedf9592aba2afd70104");
```

Because the execution is deterministic, you can now attach a debugger, add logging, or bisect the
bug without the failure evaporating.

`check_random` also prints the seed its RNG was started from:

```text
failing seed:
"
13500762202844185232
"
```

Pass that to `shuttle::check_random_with_seed`, or set `SHUTTLE_RANDOM_SEED` and re-run
`check_random`. A seed is more compact than a schedule, but it reproduces something different: a
schedule replays one specific interleaving exactly once, whereas a seed re-runs the whole randomized
search from the start, reproducing every iteration up to and including the failing one. Reach for the
schedule when you want to debug the failure, and the seed when you want to repeat the run.

Schedules for long-running tests can get large. Set `Config::failure_persistence` to
`FailurePersistence::File` to write them to disk instead of stdout, and replay with
`shuttle::replay_from_file`. A few environment variables help here too: `SHUTTLE_RANDOM_SEED` fixes
the seed used by `check_random`, `SHUTTLE_PERSIST_SEED` writes the seed out before the test runs
(useful when a test aborts rather than panics), and `SHUTTLE_BACKTRACE` prints a backtrace for every
task when Shuttle detects a deadlock.

## What Shuttle models

Shuttle provides replacements for the concurrency primitives in `std`. Anything Shuttle controls
becomes a scheduling decision it can explore and replay.

| Module | Contents |
| --- | --- |
| `shuttle::sync` | `Mutex`, `RwLock`, `Condvar`, `Barrier`, `Once`, `mpsc`, and lock poisoning (`LockResult`, `PoisonError`, `TryLockError`) |
| `shuttle::sync::atomic` | `AtomicBool`, `AtomicPtr`, all integer atomics including `AtomicI128`/`AtomicU128`, `fence`, `compiler_fence` |
| `shuttle::thread` | `spawn`, `Builder`, `JoinHandle`, `scope`, `yield_now`, `park`/`unpark`, `current`, plus the `thread_local!` macro |
| `shuttle::future` | `spawn`, `spawn_local`, `block_on`, `JoinHandle`, `AbortHandle`, `yield_now`, and `BatchSemaphore` |
| `shuttle::rand` | a drop-in replacement for `rand` 0.8, so random *data* is replayable alongside the schedule |
| `shuttle::lazy_static` | the `lazy_static!` macro |
| `shuttle::current` | task identity, labels, logical clocks (`context_switches`, `clock`), step counts |

Things worth knowing before you rely on them:

- **`Arc` is `std`'s `Arc`.** Shuttle re-exports it unchanged, so its internal atomics are not
  modeled.
- **The `atomic` implementation is unsound and may miss bugs.** Shuttle warns about this at runtime;
  `Config::silence_warnings` suppresses the warning.
- **`fence(Ordering::Relaxed)` panics**, since there is no such thing as a relaxed fence.
- **Time is not modeled.** `thread::sleep` and `park_timeout` are scheduling points, not delays; a
  test where every thread is parked with a timeout is reported as a deadlock.
- **No `OnceLock`, `LazyLock`, or `mpmc`** replacements yet.
- Vector clocks, which power the causality tracking exposed through `shuttle::current`, are behind
  the `vector-clocks` feature.

## Choosing a scheduler

The scheduler decides which runnable task to run at each scheduling point. Different policies find
different classes of bugs, at the cost of exploring more executions. Each is reachable through a
convenience function, or directly through `Runner` when you need more control.

Shuttle implements a number of *randomized concurrency testing* techniques, including
[A Randomized Scheduler with Probabilistic Guarantees of Finding Bugs](https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/asplos277-pct.pdf)
(PCT).

| Scheduler | Entry point | When to use it |
| --- | --- | --- |
| `RandomScheduler` | `check_random(f, iterations)` | The default. Picks uniformly at random among runnable tasks. |
| `PctScheduler` | `check_pct(f, iterations, depth)` | Bounds the number of preemptions to `depth`. Most real bugs need very few preemptions, so this concentrates effort where bugs actually live. |
| `DfsScheduler` | `check_dfs(f, max_iterations)` | Exhaustive depth-first search. Intractable beyond very small tests, but thorough for individual primitives. |
| `UrwRandomScheduler` | `check_urw(f, iterations)` | Uniform random walk over the schedule space. Helps when tasks have very uneven amounts of work and plain random scheduling biases away from the interesting interleavings. |
| `ReplayScheduler` | `replay(f, schedule)`, `replay_from_file(f, path)` | Reproduce a recorded failure. |
| `UncontrolledNondeterminismCheckScheduler` | `check_uncontrolled_nondeterminism(f, iterations)` | Detects tests whose behavior depends on nondeterminism Shuttle does not control, which would otherwise make failures unreproducible. See [below](#checking-for-uncontrolled-nondeterminism). |
| `RoundRobinScheduler` | — | Cycles through tasks in a fixed order. Almost never what you want. |
| `AnnotationScheduler` | — | Records an annotated schedule for [Shuttle Explorer](#shuttle-explorer). Requires the `annotation` feature. |

`Runner::new(scheduler, config).run(f)` runs a test with an explicit scheduler and configuration.
`PortfolioRunner` runs several schedulers in parallel, which is a cheap way to increase the number
of executions explored.

### Checking for uncontrolled nondeterminism

Everything above rests on Shuttle being the only source of nondeterminism in the test. When that is
not true — the test branches on a real clock, a `HashMap` iteration order, an `OsRng`, or the
environment — schedules stop being reproducible, and a failure you find may not replay.

`check_uncontrolled_nondeterminism(f, iterations)` looks for exactly that. It wraps the random
scheduler and runs each generated schedule *twice*, checking on the second run that the schedule is
still valid and that the set of runnable tasks, the yielding flag, and the random values requested
all match the first run. Any divergence panics with a `possible nondeterminism:` message describing
what differed.

It is worth running when replay does not reproduce a failure, or when adopting Shuttle on a codebase
for the first time. Two caveats: it halves the number of distinct schedules explored for a given
iteration count, since every schedule is run twice; and it is a bug-finder rather than a proof —
passing does not establish that the test is free of uncontrolled nondeterminism, only that none was
observed on the schedules tried. The usual fixes are the `shuttle::rand` and
`determinizable_collections` wrappers.

## Configuration

`Config` controls how a test runs. The knobs most likely to matter:

- `max_steps` — an upper bound on the steps (atomic regions between scheduling points) in a single
  iteration, and what to do when it's hit: `FailAfter` (the default, 1,000,000 steps) or
  `ContinueAfter`. This is your protection against livelock and unfair schedules.
- `max_time` — a wall-clock budget for the whole test. Checked between iterations, so it won't
  interrupt an iteration in progress.
- `failure_persistence` — `Print` (default), `File`, or `None`.
- `stack_size` — stack allocated per task, default `0xf000`.
- `silence_warnings` — suppresses the unsound-atomics and dropped-`lazy_static` warnings.
- `record_steps_in_span` — appends step counts to tracing spans.
- `ungraceful_shutdown_config` — whether to stop scheduling the instant a task panics
  (`immediately_return_on_panic`) rather than letting it finish unwinding.

Shuttle logs through [`tracing`](https://crates.io/crates/tracing), tagging every event with the
task that produced it, so a normal `tracing_subscriber` gives you a readable interleaving trace.

## Testing tokio code

Shuttle can run tokio applications, and this is usually how it gets applied to real services. The
integration works differently from the rest of Shuttle: instead of rewriting your imports, you swap
the `tokio` dependency itself for a Shuttle-aware wrapper crate.

### Setting it up

Add `shuttle-tokio` under the name `tokio`, and forward your own `shuttle` feature to it:

```toml
[features]
shuttle = [
   "tokio/shuttle",
]

[dependencies]
tokio = { package = "shuttle-tokio", version = "1" }
```

Every `use tokio::...` in your codebase stays exactly as it is. Without the `shuttle` feature you
get real tokio, unchanged. With it, `shuttle-tokio` re-exports Shuttle's implementations in tokio's
place:

```rust
cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_tokio_impl::*;
    } else {
        pub use tokio::*;
    }
}
```

`shuttle-tokio` mirrors tokio's feature set one-to-one (`full`, `rt`, `rt-multi-thread`, `sync`,
`macros`, `time`, `net`, `fs`, `io-util`, `signal`, `test-util`, and the rest), and each feature
forwards to both real tokio and the Shuttle implementation. Nothing is enabled by default.

### Writing a test

`#[tokio::test]` works, and accepts tokio's usual arguments:

```rust
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::test]
async fn concurrent_increment() {
    let lock = Arc::new(Mutex::new(0usize));
    let lock2 = Arc::clone(&lock);

    let jh = tokio::spawn(async move {
        *lock2.lock().await += 1;
    });

    *lock.lock().await += 1;
    jh.await.unwrap();

    assert_eq!(*lock.lock().await, 2);
}
```

Under the `shuttle` feature this expands into a Shuttle run of the test body rather than a single
tokio execution. It defaults to 100 iterations under the random scheduler, and raises the per-task
stack size and step budget (to `0xF0000` and 10,000,000 steps respectively) because real service
code tends to need more of both than the Shuttle defaults provide. Tests returning `Result` and
tests marked `#[should_panic]` both behave as you'd expect.

### What is modeled

Four of tokio's modules have real Shuttle implementations, meaning the operations in them are
scheduling points that Shuttle controls and replays.

**`tokio::sync`** is the most complete. `Mutex` (with `OwnedMutexGuard`), `RwLock` (with owned
guards), `Semaphore`, `Notify`, `OnceCell`, and the `broadcast`, `mpsc`, `oneshot`, and `watch`
channels are all backed by Shuttle primitives. `tokio::select!` works over them, including the
combinations that used to trip Shuttle's internal bookkeeping.

**`tokio::task`** provides `spawn`, `JoinHandle`, `JoinError`, `AbortHandle`, `JoinSet`, `Builder`,
`id()`/`try_id()`, `yield_now`, and a `coop` module. Task ids are Shuttle `TaskId`s, so the ids in
your logs line up with the ids in Shuttle's traces.

**`tokio::runtime`** exists mainly so that code which constructs a runtime still compiles. `Runtime`,
`Builder`, and `Handle` are thin shims: `block_on` and `spawn` forward to Shuttle, and everything
else is a no-op that returns `self`. In particular, `Builder::new_multi_thread()` is
`new_current_thread()`, and `worker_threads`, `enable_all`, `enable_time`, `start_paused`, and
`thread_name` are all ignored. This is not a limitation in practice: Shuttle explores concurrency by
interleaving tasks on one thread, so a "multi-threaded" runtime and a current-thread runtime give the
same coverage. `RuntimeMetrics` returns zeros, and `Handle::dump()` and `runtime_flavor()` are
unimplemented.

**`tokio::time`** is where the semantics diverge most, so it gets its own section.

### Time and timeouts

Shuttle does not model time. Instead, time-based operations become scheduling points, and timeouts
are fired explicitly by the test.

- `sleep` and `sleep_until` yield to the scheduler and then complete, regardless of the duration
  requested. A sleep whose deadline is more than a year out never completes, which is the idiom for
  "block this task forever".
- `advance` is a bare yield. `pause` and `resume` are no-ops.
- `interval` ticks by yielding. Ticks are unbounded by default; set `SHUTTLE_INTERVAL_TICKS` to cap
  them, or `0` for an interval that never ticks. `Interval::period()` warns, because reading the
  period suggests logic that depends on real elapsed time and therefore won't replay.
- `Instant` is the real `tokio::time::Instant`, backed by the real clock. Branching on it introduces
  nondeterminism Shuttle can't reproduce.
- `timeout` and `timeout_at` **ignore their duration entirely.** A wrapped future never times out on
  its own.

That last point is deliberate. Rather than guessing when a timeout should fire, Shuttle lets the test
decide, using `trigger_timeouts` to expire the timeouts of tasks matching a predicate over their
[labels](https://docs.rs/shuttle/latest/shuttle/current/index.html). Timeouts become something you
inject on purpose, which makes timeout-recovery paths reachable and replayable.

Here it is applied to Dining Philosophers, where expiring one philosopher's `lock()` is what breaks
the deadlock cycle:

```rust
use shuttle::current::{me, set_label_for_task, Labels};
use tokio::sync::Mutex;
use tokio::time::{timeout, trigger_timeouts, Duration};

#[derive(Clone, Debug)]
struct Philosopher(usize);

// The i'th philosopher grabs forks i and i+1, modulo count.
for i in 0..count {
    let (left, right) = (forks[i].clone(), forks[(i + 1) % count].clone());
    handles.push(tokio::spawn(async move {
        let _ = set_label_for_task(me(), Philosopher(i));
        let l = timeout(Duration::from_secs(1), left.lock()).await;
        let r = timeout(Duration::from_secs(1), right.lock()).await;
        l.is_err() || r.is_err()
    }));
}

// Expire the middle philosopher's timeouts. Without this, Shuttle reports a deadlock.
trigger_timeouts(move |labels: &Labels| {
    labels.get::<Philosopher>().expect("label not set").0 == count / 2
});
```

Triggers apply to timeouts already in flight as well as ones registered later. `clear_triggers`
resets them.

### What is not modeled

Some modules are re-exported from real tokio so that code compiles under the `shuttle` feature. They
are not Shuttle-aware, and using them inside a Shuttle test will misbehave — typically by blocking on
real I/O that Shuttle's single-threaded execution can never make progress on.

- **`io`, `net`, `fs`, and `signal`** are all real tokio.
- **`task_local!`** is real tokio's, and currently gives *shared* storage across all Shuttle tasks.
  It is not merely unmodeled, it is wrong; avoid it under Shuttle.
- **`spawn_blocking`** becomes an ordinary Shuttle task. There is no separate blocking pool, and the
  work is subject to normal scheduling.
- **`join!`, `try_join!`, and `#[tokio::main]`** come from real tokio-macros. `join!` and `try_join!`
  are fine, since they only compose futures. `#[tokio::main]` is untested under Shuttle; use
  `#[tokio::test]`.

The canonical list of unsupported constructs is tracked in
[issue #241](https://github.com/awslabs/shuttle/issues/241). If you hit a gap, an issue or a PR is
very welcome.

### Environment variables

Tokio tests read their configuration from the environment, which means you can re-run a failing test
under a different scheduler without touching the code:

| Variable | Effect |
| --- | --- |
| `SHUTTLE_ITERATIONS` | Number of iterations to run, overriding the default of 100. |
| `SHUTTLE_TIMEOUT_SECS` | Run for a fixed wall-clock budget instead of a fixed iteration count. Takes precedence over `SHUTTLE_ITERATIONS`. |
| `SHUTTLE_SCHEDULER` | `PCT` for the PCT scheduler, `PORTFOLIO` to run PCT and random in parallel; anything else uses random. |
| `SHUTTLE_PCT_MAX_DEPTH` | PCT bug depth, default 3. |
| `SHUTTLE_TRACE_DIR` | Write failing schedules into this directory instead of printing them. |
| `SHUTTLE_TRACE_FILE` | Replay the schedule in this file, once. |
| `SHUTTLE_INTERVAL_TICKS` | Cap on the number of ticks each `Interval` produces. |
| `SHUTTLE_HIDE_TRACE` | Install a subscriber that swallows all output, so `RUST_LOG=trace` can be used for replay without drowning randomized runs in logs. |

The usual workflow is to find a failure and then replay it:

```console
$ SHUTTLE_TRACE_DIR=./failures cargo test --release --features shuttle -- my_test
$ RUST_BACKTRACE=1 SHUTTLE_TRACE_FILE=./failures/schedule000.txt \
    cargo test --release --features shuttle -- my_test
```

### Companion crates

The tokio ecosystem crates follow the same swap pattern, each with its own `shuttle` feature:

```toml
[dependencies]
tokio = { package = "shuttle-tokio", version = "1" }
tokio-stream = { package = "shuttle-tokio-stream", version = "0.1" }
tokio-util = { package = "shuttle-tokio-util", version = "0.7" }
tokio-retry = { package = "shuttle-tokio-retry", version = "0.3" }
```

## Wrappers for other crates

The same dependency-swap approach covers several other crates commonly found in concurrent code.
Each behaves as the original without the `shuttle` feature, and as a Shuttle-compatible
implementation with it.

| Wrapper | Wraps |
| --- | --- |
| `shuttle-parking_lot` | `parking_lot` |
| `shuttle-dashmap` | `dashmap` |
| `shuttle-async-stream` | `async-stream` |
| `shuttle-lazy_static` | `lazy_static` |
| `shuttle-rand` | `rand` 0.8 |

Two wrappers work slightly differently, because there is no upstream crate to rename:

- `shuttle-sync` covers `std::sync`. Depend on it directly and import from `shuttle_sync::sync`
  rather than `std::sync`.
- `determinizable_collections` provides `HashMap` and `HashSet` with reproducible iteration order,
  which Shuttle needs in order to replay a failure. It is gated on a `deterministic` feature rather
  than a `shuttle` one, so it can also be turned on in pre-production builds to make failures
  elsewhere easier to reproduce.

See [wrappers/README.md](/wrappers/README.md) for the full list, the versioning scheme, and guidance
on managing the `shuttle` feature across a large workspace.

## Shuttle Explorer

Shuttle can emit an annotated schedule describing everything that happened during a failing
execution, which the Shuttle Explorer VS Code extension renders as an interactive timeline of tasks
and the operations between them. Build with the `annotation` feature and use `AnnotationScheduler`
or `annotate_replay`. See [shuttle-explorer/README.md](/shuttle-explorer/README.md).

## Repository layout

`shuttle` is the crate you depend on; it re-exports the three crates underneath it, which are split
out so that tools can depend on just the parts they need.

| Path | Contents |
| --- | --- |
| [`shuttle/`](/shuttle) | The public crate. Also home to the integration test suite and benchmarks. |
| [`shuttle-engine/`](/shuttle-engine) | Core runtime, the `Scheduler` trait, `Config`, `Runner`, `PortfolioRunner`. |
| [`shuttle-schedulers/`](/shuttle-schedulers) | The built-in schedulers and the `check`/`replay` entry points. |
| [`shuttle-std/`](/shuttle-std) | The `std` replacement primitives: `sync`, `thread`, `future`. |
| [`wrappers/`](/wrappers) | Drop-in wrappers for third-party crates. |
| [`shuttle-explorer/`](/shuttle-explorer) | VS Code extension for exploring annotated schedules. |

`shuttle/tests/demo` is a good place to see Shuttle applied to real bugs, including a classic
`BoundedBuffer` deadlock and an async deadlock caused by a lock held across a `match`.

## Contributing

See [CONTRIBUTING](CONTRIBUTING.md). Bug reports, and especially additions to the wrappers, are
welcome.

## License

This project is licensed under the Apache-2.0 License.

## Security

See [CONTRIBUTING](CONTRIBUTING.md#security-issue-notifications) for more information.
