//! This crate contains Shuttle's implementation of the [`crossbeam-utils`] crate, version 0.8. Do
//! not depend on this crate directly. Use the [`shuttle-crossbeam-utils`] crate, which
//! conditionally exposes this implementation with the `shuttle` feature or the original crate
//! without it.
//!
//! [`crossbeam-utils`]: <https://crates.io/crates/crossbeam-utils>
//! [`shuttle-crossbeam-utils`]: <https://crates.io/crates/shuttle-crossbeam-utils>
//!
//! The API is crossbeam-utils 0.8.23's: [`CachePadded`], [`Backoff`], [`atomic::AtomicCell`],
//! [`atomic::AtomicConsume`], [`sync::Parker`], [`sync::Unparker`], [`sync::ShardedLock`],
//! [`sync::WaitGroup`] and [`thread`]'s scoped threads.
//!
//! # What each item is
//!
//! | Item | Under Shuttle |
//! |---|---|
//! | [`CachePadded`] | crossbeam-utils's own: it aligns a value and has nothing to schedule. |
//! | [`Backoff`] | `spin` and `snooze` each take one scheduling point, so a busy-wait lets the task it waits for run. |
//! | [`atomic::AtomicCell`] | one scheduling point and then an uninterrupted operation, so it is atomic whatever `T` is, with the causality of one of Shuttle's atomics. |
//! | [`atomic::AtomicConsume`] | a trait of this crate, implemented for Shuttle's atomics and for `std`'s. `load_consume` is a load, which Shuttle models as `SeqCst`. |
//! | [`sync::ShardedLock`] | Shuttle's `RwLock`, which is what the sharding optimizes; the shards are not observable. |
//! | [`sync::WaitGroup`] | Shuttle's `Mutex`, `Condvar` and atomics, so waiting blocks a task. |
//! | [`sync::Parker`] | its own token, with a blocked Shuttle task; `park_timeout` times out only when no task can run. |
//! | [`thread::scope`] | `shuttle::thread::scope`, so the threads are tasks and the scope blocks the parent until they finish. |
//!
//! # Scheduling points
//!
//! An operation another task can observe takes one, as the real crate's synchronizes:
//!
//! - `Backoff::spin` and `Backoff::snooze`: one each, rather than the real crate's 2^step spin-loop
//!   hints, which would lengthen a schedule without exploring anything new. `Backoff::new`,
//!   `reset` and `is_completed` take none.
//! - Every [`atomic::AtomicCell`] operation takes one, and so does `load_consume`. `AtomicCell`'s
//!   `new`, `into_inner`, `as_ptr` and `is_lock_free` take none.
//! - [`sync::ShardedLock`]'s `read`, `write`, `try_read` and `try_write` take one, as Shuttle's
//!   `RwLock` does, and so does dropping a guard. `is_poisoned` and `get_mut` take none.
//! - [`sync::Parker::park`] and [`sync::Unparker::unpark`] take one. `WaitGroup::wait` and dropping
//!   the last `WaitGroup` take one, through the `Mutex` and `Condvar` they use.
//! - Spawning a scoped thread takes one, inside Shuttle's `spawn`. `scope` blocks the parent task
//!   until its threads have finished.
//!
//! # Time
//!
//! [`sync::Parker::park_timeout`] and [`sync::Parker::park_deadline`] measure time on
//! `shuttle-time`'s clock, and **time out only when no task can run**, which then moves the clock
//! to the deadline (see [`shuttle_time_inner::timer_support::block_until`]). A park therefore never
//! returns early in a schedule where the unparking task could still have run, and a loop that parks
//! with a timeout while it waits for one is not reported as deadlocked. `park_deadline` takes
//! `shuttle-time`'s `Instant`, not `std::time::Instant`.
//!
//! # Differences from crossbeam-utils
//!
//! Each item's own documentation lists its differences. The ones to know:
//!
//! - Every operation has to run inside a Shuttle test. `AtomicCell::new`, `Backoff::new`,
//!   `WaitGroup::new` and `ShardedLock::new` do not, so a `static` of any of them is fine.
//! - A primitive that outlives the execution it was used in starts the next execution over: a
//!   `Parker` with no token and nobody waiting, an [`atomic::AtomicCell`] with no clock, and a
//!   [`sync::ShardedLock`] or [`sync::WaitGroup`] as Shuttle's own primitives under them do. The
//!   one exception is an `AtomicCell`'s **value**, which a reset would have to put back as the cell
//!   was created with, and a cell of a `T` that is neither `Copy` nor `Clone` has no copy of it.
//! - `AtomicCell::into_inner` is not a `const fn`, because the cell holds a clock whose destructor
//!   a `const fn` cannot run. `new` and `as_ptr` are `const`, as crossbeam's are.
//! - A panic in a scoped thread fails the test, as a panic in any Shuttle task does, so
//!   `thread::scope` does not return the `Err` that crossbeam's collects the panics into.
//! - `thread::ScopedJoinHandle` has no `JoinHandleExt`: a Shuttle task has no OS thread of its own.
//! - A `Parker` has no spurious wakeups.

pub mod atomic;
mod backoff;
mod generation;
pub mod sync;
pub mod thread;

pub use crate::backoff::Backoff;
// `CachePadded` aligns a value to a cache line and has no behavior to model, so it is
// crossbeam-utils's own type in both modes, which also keeps conversions at a crate boundary free.
pub use crossbeam_utils::CachePadded;
