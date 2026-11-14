# Shuttle support for `arc-swap`

This crate contains the implementation that enables testing of applications that use
[arc-swap](https://crates.io/crates/arc-swap) 1.9 with Shuttle. It should not be depended on
directly, depend on `shuttle-arc-swap` instead.

## Implementation

arc-swap keeps the stored `Arc` in an `AtomicPtr` and protects a loaded value with per-thread
"debt" slots, so that a read costs no reference count and a write waits for the readers that still
owe one. None of that is a scheduling point, so Shuttle never runs another task around a `load` or
a `store`, and the debt slots are thread-locals of the one OS thread an execution runs on, which
every task of the execution would share.

This implementation keeps the same pointer, and:

* **Every operation is one scheduling point and one step.** `load`, `load_full`, `store`, `swap`
  and `compare_and_swap` take a scheduling point and then read or replace the pointer without
  another one, so each is atomic with respect to the other tasks and the operations are
  linearizable. `rcu` is arc-swap's own loop over `load` and `compare_and_swap`, so it takes their
  points, two per attempt.
* **A loaded value is a clone of the smart pointer.** `Guard` holds an `Arc` (or whatever the
  stored smart pointer is), so the value stays alive and readable for as long as the guard does,
  whatever the other tasks store: no use after free. Nothing is deferred, so a write never waits
  for a reader.
* **The stored pointer lives in the running execution**, keyed by an id the storage assigns itself
  on first use, so a `static` storage holds the value it was constructed with at the start of every
  execution, and two Shuttle tests that run at the same time and share one `static` do not see each
  other's values.
* **Causality is tracked**, as it is for Shuttle's atomics: a task that loads a value is causally
  after the store that put it there.

## Limitations

* **`Arc::strong_count` is one higher while a `Guard` is alive.** arc-swap's `load` leaves the
  count alone (that is what the debt slots are for); here it clones the smart pointer. Code that
  asserts on `strong_count` while holding a guard sees a different number.
* **A storage keeps a reference count for its initial value** until the storage itself is dropped,
  so that every execution can start from it. arc-swap gives that count up as soon as the value is
  replaced.
* **Every other value is freed at the end of the execution that stored it**, at the latest: the
  execution's state gives up its count when Shuttle tears the execution down, as
  `shuttle::lazy_static!` does. Replacing a value frees it as soon as the last `Guard` and the last
  clone of it are gone, as in arc-swap.
* **The strategies do nothing.** `DefaultStrategy` and `IndependentStrategy` name the one strategy
  there is, whose `Protected` type is the smart pointer itself. The traits stay sealed, as in
  arc-swap, and the `rwlock-strategy` and `internal-test-strategies` features keep their names so
  that code which spells them out still compiles; neither takes a lock or fills a slot.
* **`no_std` is not supported:** Shuttle needs `std`, so the `experimental-thread-local` feature
  (which makes arc-swap `no_std` and needs nightly) has no effect here.
* An `ArcSwapAny` that was created inside one execution and used in another holds the value it was
  constructed with, not the one the earlier execution stored. A value stored in an execution never
  leaks into the next one.
