//! This crate contains Shuttle's internal implementation of the [`crossbeam-deque`] crate.
//! Do not depend on this crate directly. Use the [`shuttle-crossbeam-deque`] crate, which
//! conditionally exposes this implementation with the `shuttle` feature or the original crate
//! without it.
//!
//! [`crossbeam-deque`]: <https://crates.io/crates/crossbeam-deque>
//! [`shuttle-crossbeam-deque`]: <https://crates.io/crates/shuttle-crossbeam-deque>
//!
//! # The model
//!
//! [`Worker`], [`Stealer`] and [`Injector`] have crossbeam-deque 0.8.8's API, and [`Steal`] is
//! crossbeam-deque's own type. Each queue is a sequence of tasks, and each operation reads or
//! changes it at a scheduling point, where crossbeam's queue performs the atomic operations that
//! other threads can observe:
//!
//! | Operation | Scheduling points |
//! |---|---|
//! | `Worker::push`, `Worker::pop`, `Injector::push`, and every `len` and `is_empty` | 1 |
//! | `steal` that finds the queue empty | 1, and returns [`Steal::Empty`] |
//! | `steal` otherwise | 2: reading the task, and taking it |
//! | a batch steal from a FIFO worker or an injector | 1 if the queue is empty, 2 otherwise |
//! | a batch steal from a LIFO worker | 1 if the queue is empty; otherwise 1, then 1 per task it tries to take |
//! | `new_fifo`, `new_lifo`, `stealer`, `Stealer::clone`, `Injector::new`, `Default`, `Debug`, drop | 0 |
//!
//! A batch steal whose destination is the source's own worker does no stealing: it is
//! `Worker::is_empty` (`steal_batch*`) or `Worker::pop` (`steal_batch*_and_pop`), as in crossbeam.
//!
//! # When a steal returns `Steal::Retry`
//!
//! crossbeam's steals read the task at the front, and then take it with a compare-and-swap of the
//! front index. The compare-and-swap fails, and the steal returns [`Steal::Retry`], when another
//! operation moved the front index in between: another steal, a FIFO worker's pop, or a LIFO
//! worker's pop of the last task. A stealer of a worker also retries when the worker swapped its
//! buffer for one of another size in between, which crossbeam does when a push finds the buffer
//! full, when a pop leaves it a quarter full (never below 64), and when a batch steal into the
//! worker needs the room.
//!
//! The model does the same. A steal reads the queue at its first scheduling point and takes the
//! task at its second, and it returns `Steal::Retry` exactly when one of those operations ran in
//! between; the buffer's size is tracked as crossbeam would size it. So whether a steal retries is
//! decided by the schedule, which Shuttle explores, records and replays, `check_dfs` included. A
//! steal never retries when nothing ran in between, as crossbeam's never does on hardware whose
//! compare-and-swap does not fail spuriously: a test that steals from a queue no one else touches
//! sees what it would see with crossbeam. From a LIFO worker, crossbeam steals a batch one task at
//! a time, and stops early when another operation gets in the way; the model does too, with a
//! scheduling point per task.
//!
//! How many tasks a batch steal takes is crossbeam's choice: half of them, rounded up, and no more
//! than the limit (32 by default; 33 for `Injector::steal_batch_and_pop`); from an injector, also
//! no more than are left in the block of 63 that crossbeam's queue has at its head.
//!
//! Every operation also makes the calling task and the queue exchange vector clocks, as an
//! operation on one of Shuttle's atomics does.
//!
//! # State per execution
//!
//! A queue's tasks belong to the running execution: they live in the execution's storage, and the
//! queue holds only an identifier. So a queue that outlives an execution, such as an [`Injector`]
//! the test closure captured, starts every execution empty, which keeps an execution from depending
//! on the ones before it. Tasks still in a queue when an execution ends are dropped with the
//! execution; a queue that is dropped during an execution drops its tasks right away.
//!
//! # Differences from crossbeam-deque
//!
//! - The operations must run inside a Shuttle execution, and panic outside of one with Shuttle's
//!   "Are you accessing a Shuttle primitive outside of a Shuttle test?". Constructing a queue, making
//!   stealers, `Debug` and dropping work anywhere.
//! - Nothing waits. crossbeam's injector spins until a concurrent push has finished writing the
//!   task it took, and until a push has linked the next block; here each push finishes in its step.
//! - crossbeam's compare-and-swaps of the injector's head may fail spuriously on hardware with
//!   load-linked/store-conditional instructions, which makes a steal retry with nothing in between.
//!   The model does not do that, as Shuttle's atomics do not.
//! - A queue that is leaked (with `mem::forget` or a reference cycle) still has its tasks dropped
//!   when the execution ends, where crossbeam would leak them.
//! - The queues do not take part in Shuttle's annotated schedules as named resources, and do not use
//!   epoch-based reclamation (crossbeam's swaps buffers through crossbeam-epoch).

// The scheduling points and the storage come from `shuttle-engine`. The dependency on `shuttle` only
// makes sure that the `shuttle` a test runs with is built on that engine (0.9.2 or later).
use shuttle as _;

mod deque;
mod per_execution;

pub use crossbeam_deque_orig::Steal;
pub use deque::{Injector, Stealer, Worker};

/// The second half of this crate's keys in the execution storage, which tells them apart from the
/// keys of Shuttle's own primitives and of the other wrappers.
const STORAGE_TAG: usize = 0x6362_6471; // "cbdq"
