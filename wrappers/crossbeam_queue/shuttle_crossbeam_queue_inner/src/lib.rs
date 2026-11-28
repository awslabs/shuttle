//! This crate contains Shuttle's internal implementation of the [`crossbeam-queue`] crate.
//! Do not depend on this crate directly. Use the [`shuttle-crossbeam-queue`] crate, which
//! conditionally exposes this implementation with the `shuttle` feature or the original crate
//! without it.
//!
//! [`crossbeam-queue`]: <https://crates.io/crates/crossbeam-queue>
//! [`shuttle-crossbeam-queue`]: <https://crates.io/crates/shuttle-crossbeam-queue>
//!
//! # The model
//!
//! [`ArrayQueue`] and [`SegQueue`] have crossbeam-queue 0.3.14's API. Each is a FIFO queue whose
//! operations happen in one step at a scheduling point, where the real queue performs atomic
//! operations that other threads can observe:
//!
//! | Operation | Scheduling points |
//! |---|---|
//! | `push`, `force_push`, `pop`, `len`, `is_empty`, `is_full` | 1 |
//! | `push_mut`, `pop_mut` (they take `&mut self`), `capacity`, `new`, `Default`, `Debug`, `into_iter`, drop | 0 |
//!
//! So both queues are linearizable, as crossbeam's are, and Shuttle explores every order in which
//! concurrent operations take effect. Every operation also makes the calling task and the queue
//! exchange vector clocks, as an operation on one of Shuttle's atomics does.
//!
//! Neither queue blocks. crossbeam's queues spin in a few places to wait for a concurrent operation
//! to finish writing (a `pop` that finds a slot whose `push` has claimed it but not yet written it,
//! say). Here every operation finishes in its step, so there is nothing to wait for, and the
//! outcome is one of those that crossbeam's queue could have produced once the wait was over.
//!
//! # State per execution
//!
//! A queue's contents belong to the running execution: they live in the execution's storage, and
//! the queue holds only an identifier and its capacity. So a queue that outlives an execution, a
//! `static` [`SegQueue`] or a queue that the test closure captured, starts every execution empty,
//! which keeps an execution from depending on the ones before it. Values still in such a queue when
//! an execution ends are dropped with the execution, as Shuttle drops the values of
//! `lazy_static!`s; a queue that is dropped during an execution drops its values right away.
//!
//! # Differences from crossbeam-queue
//!
//! - The operations must run inside a Shuttle execution, and panic outside of one with Shuttle's
//!   "Are you accessing a Shuttle primitive outside of a Shuttle test?". Only constructing a queue,
//!   `capacity`, `Debug` and dropping work anywhere. `into_iter` outside of an execution yields
//!   nothing.
//! - `ArrayQueue::new` does not allocate its buffer up front; the queue grows to its capacity as
//!   values arrive. It still panics on a capacity of zero, and on one whose lap computation
//!   overflows, but not when a buffer of that capacity cannot be allocated.
//! - A queue that is leaked (with `mem::forget` or a reference cycle) still has its values dropped
//!   when the execution ends, where crossbeam would leak them. Do not combine that with values
//!   whose destructors read data the queue borrows.
//! - The queues do not take part in Shuttle's annotated schedules as named resources.

// The scheduling points and the storage come from `shuttle-engine`. The dependency on `shuttle` only
// makes sure that the `shuttle` a test runs with is built on that engine (0.9.2 or later).
use shuttle as _;

mod array_queue;
mod per_execution;
mod seg_queue;

pub use array_queue::ArrayQueue;
pub use seg_queue::SegQueue;

/// The second half of this crate's keys in the execution storage, which tells them apart from the
/// keys of Shuttle's own primitives and of the other wrappers.
const STORAGE_TAG: usize = 0x6362_7175; // "cbqu"
