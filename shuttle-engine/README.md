# shuttle-engine

This crate contains the core runtime of the [Shuttle](https://crates.io/crates/shuttle) concurrency
testing tool. It provides the building blocks for creating functionality on top of Shuttle: the
execution engine, the `Scheduler` trait, and the primitives that schedulers and `std` replacements
are built from. Reach for it when you want to build something new atop the runtime, such as a custom
scheduler or a Shuttle-compatible version of a concurrency primitive.

## Contents

This crate holds the machinery that executes and controls a Shuttle test:

* `runtime` — the execution engine: tasks, threads (built on
  [corosensei](https://crates.io/crates/corosensei) continuations), thread-local storage, vector
  clocks, and failure reporting.
* `scheduler` — the `Scheduler` trait that every scheduler implements, along with schedule
  serialization, metrics, and the data-source traits used to control nondeterministic values. The
  built-in schedulers themselves live in
  [shuttle-schedulers](https://crates.io/crates/shuttle-schedulers).
* `config` — `Config` and the associated knobs (`MaxSteps`, `FailurePersistence`,
  `UngracefulShutdownConfig`, ...).
* `future` — the async primitives underpinning Shuttle's executor, including `BatchSemaphore`.
* `current`, `hint`, `sync_types`, `thread_support` — the supporting APIs that
  [shuttle-std](https://crates.io/crates/shuttle-std) builds its `std` mirrors on top of.

## Features

* `vector-clocks` — track causality between tasks. Required for the `current` clock APIs.
* `annotation` — emit annotated schedules for the
  [Shuttle Explorer](https://github.com/awslabs/shuttle/tree/main/shuttle-explorer) extension.

## Stability

This crate exposes considerably more of Shuttle's internals than the `shuttle` crate does, and that
surface evolves alongside the runtime. Expect it to change more freely than a typical crate's public
API, and pin a version if you build against it.
