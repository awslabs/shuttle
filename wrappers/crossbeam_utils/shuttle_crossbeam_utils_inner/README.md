# Shuttle support for `crossbeam-utils`

This crate contains the implementation that enables testing of applications that use [crossbeam-utils](https://crates.io/crates/crossbeam-utils) with Shuttle. It should not be depended on directly, depend on `shuttle-crossbeam-utils` instead.

## Implementation

The crate has crossbeam-utils 0.8.23's API: `CachePadded`, `Backoff`, `atomic::{AtomicCell, AtomicConsume}`, `sync::{Parker, Unparker, ShardedLock, ShardedLockReadGuard, ShardedLockWriteGuard, WaitGroup}` and `thread`'s scoped threads.

* **`CachePadded`** is crossbeam-utils's own type: it aligns a value and has nothing to schedule, which also keeps it the same type at a crate boundary.
* **`Backoff`**: `spin` and `snooze` take one scheduling point each, rather than the real crate's 2^step spin-loop hints, which would lengthen a schedule without exploring anything new. The step counter is crossbeam's, so `is_completed` turns true after the same number of calls.
* **`AtomicCell`**: one scheduling point and then an uninterrupted operation, so every operation is atomic whatever `T` is, where the real cell uses an atomic instruction when it can and a lock otherwise. A stored value carries the storing task's clock, which a task that loads it joins. `is_lock_free` reports what the real cell would do, so that code which branches on it behaves the same.
* **`AtomicConsume`** is a trait of this crate, so that it can be implemented for Shuttle's atomics, which is what a crate under the `shuttle` feature has; it is implemented for `std`'s too. `load_consume` is a load, which Shuttle models as `SeqCst`.
* **`ShardedLock`** is Shuttle's `RwLock`, which is what the sharding optimizes: the shards are not observable, and one lock explores fewer schedules.
* **`WaitGroup`** uses Shuttle's `Mutex`, `Condvar` and atomics, so a wait blocks a task and an execution that never drops a clone is reported as a deadlock.
* **`Parker`** keeps crossbeam's token semantics, with its own state rather than Shuttle's `thread::park` token, and blocks a Shuttle task.
* **`thread::scope`** is `shuttle::thread::scope`: the threads are tasks, the scope blocks the parent task until they have finished, and a child is given a scope of its own so that it can spawn into the same scope.

## Time

`Parker::park_timeout` and `park_deadline` measure time on `shuttle-time`'s clock, and **time out only when no task can run**, which then moves the clock to the deadline. A park therefore never returns early in a schedule where the unparking task could still have run, and a loop that parks with a timeout while it waits for one is not reported as deadlocked. `park_deadline` takes `shuttle-time`'s `Instant`, not `std::time::Instant`.

## Limitations

* Every operation must run inside a Shuttle test. The constructors need not, so a `static` of any of these is fine.
* A primitive that outlives the execution it was used in starts the next execution over, except for an **`AtomicCell`'s value**: a reset would have to put back the value the cell was created with, and a cell of a `T` that is neither `Copy` nor `Clone` has no copy of it. A `static AtomicCell` therefore carries what the last execution stored into the next one, as a `static` of one of Shuttle's own atomics does today. Its clock does reset. Code that needs the value to reset puts the cell in a `shuttle::lazy_static!`.
* `AtomicCell::into_inner` is not a `const fn`, because the cell holds a clock whose destructor a `const fn` cannot run. `new` and `as_ptr` are `const`, as crossbeam's are.
* A panic in a scoped thread fails the test, as a panic in any Shuttle task does, so `thread::scope` does not return the `Err` that crossbeam's collects the panics into. `ScopedJoinHandle` has no `JoinHandleExt`: a Shuttle task has no OS thread of its own.
* `ShardedLock` inherits Shuttle's `RwLock`'s rule that a task which already holds the read lock cannot take it again, which `std::sync::RwLock` documents as something that "might panic".
* A `Parker` has no spurious wakeups.
