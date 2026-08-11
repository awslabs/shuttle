# shuttle-std

This crate contains Shuttle-compatible mirrors of the `std` concurrency primitives, used by the
[Shuttle](https://crates.io/crates/shuttle) concurrency testing tool. Each type here shows how to
express a `std` API in terms of the [shuttle-engine](https://crates.io/crates/shuttle-engine)
runtime, which makes this a useful reference for building Shuttle-compatible versions of other
concurrency primitives.

Most codebases should get these primitives through the
[shuttle-sync](https://crates.io/crates/shuttle-sync) wrapper rather than from here. That wrapper
exposes either `std::sync` or the Shuttle-compatible implementation depending on a feature flag, so
the same code can run both with and without Shuttle and no imports need to change.

## Contents

Each type mirrors its `std` counterpart's API, but defers to the Shuttle runtime in
[shuttle-engine](https://crates.io/crates/shuttle-engine) so that the scheduler can control and
observe every operation:

* `sync` — `Mutex`, `RwLock`, `Condvar`, `Barrier`, `Once`, `mpsc` channels, and the `atomic`
  types (including 128-bit atomics).
* `thread` — thread spawning, joining, parking, scoped threads, and thread-local storage.
* `future` — the async equivalents: task spawning, `block_on`, and `yield_now`.

Because these are drop-in mirrors rather than the real primitives, they only work inside a Shuttle
test. Note also that they cover the concurrency surface of `std` rather than all of it, so code that
uses parts of `std::sync` which Shuttle does not model will need to keep taking those from `std`.
