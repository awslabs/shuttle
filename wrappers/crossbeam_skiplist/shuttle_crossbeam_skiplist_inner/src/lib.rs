//! This crate contains Shuttle's internal implementation of the [`crossbeam-skiplist`] crate.
//! Do not depend on this crate directly. Use the [`shuttle-crossbeam-skiplist`] crate, which
//! conditionally exposes this implementation with the `shuttle` feature or the original crate
//! without it.
//!
//! [`crossbeam-skiplist`]: <https://crates.io/crates/crossbeam-skiplist>
//! [`shuttle-crossbeam-skiplist`]: <https://crates.io/crates/shuttle-crossbeam-skiplist>
//!
//! # The model
//!
//! [`SkipMap`] and [`SkipSet`], with their `Entry`, `Iter`, `Range` and `IntoIter` types, have
//! crossbeam-skiplist 0.1.3's API. A map is a sorted sequence of entries, and each operation reads or
//! changes it at a scheduling point, where crossbeam's lock-free skip list performs the atomic
//! operations that other threads can observe. A set is a map whose values are `()`, as crossbeam's
//! is.
//!
//! | Operation | Scheduling points |
//! |---|---|
//! | `len`, `is_empty`, `front`, `back`, `get`, `contains_key`, `lower_bound`, `upper_bound`, `remove`, `pop_front`, `pop_back`, `get_or_insert` | 1 |
//! | `insert` and `compare_insert` of a key that is not in the map, or that `compare_fn` keeps | 1 |
//! | `insert` and `compare_insert` that replace an entry | 2: removing the old entry, then inserting the new one |
//! | `get_or_insert_with` of a key that is in the map | 1 |
//! | `get_or_insert_with` otherwise | 2: looking, then (after `value_fn`) inserting, or returning an entry another thread inserted meanwhile |
//! | `clear` | 1 per entry it removes, and 1 per batch of 100 |
//! | `Entry::is_removed`, `next`, `prev`, `move_next`, `move_prev`, `remove` | 1 |
//! | `Iter::next`, `next_back`, `Range::next`, `next_back` | 1 |
//! | `new`, `Default`, `iter`, `range`, `from_iter`, `into_iter`, `Entry::key`, `value`, `clone`, `Debug`, drop | 0 |
//!
//! Every operation also makes the calling task and the map exchange vector clocks, as an operation
//! on one of Shuttle's atomics does.
//!
//! # What is exact, and what is not
//!
//! Exact, as in crossbeam:
//!
//! - **Entries are reference-counted.** An `Entry` keeps its key and value readable after the entry
//!   is removed, and `is_removed` tells. `Entry::remove` returns `true` only for the call that
//!   removed it. The value is dropped once the map and every `Entry` have let go of it.
//! - **Iterators are not snapshots.** Each step of an `Iter` or a `Range` finds the entry after the
//!   last one it returned (before, from the back) in the map as it is then: it sees entries
//!   inserted ahead of it, skips entries removed before it got to them, and stops where its other
//!   end has got to. Removing entries while iterating, the iterator's own included, is allowed.
//! - **Replacing an entry takes two steps**, so for a moment the map holds neither the old entry nor
//!   the new one, and a concurrent `get` can miss the key.
//! - **`get_or_insert_with` runs `value_fn` outside of the map**, so another thread may insert the
//!   key first, and the value is then discarded, as crossbeam documents.
//! - **`clear` removes one entry at a time** from the front, starting over every 100, so entries
//!   inserted meanwhile behind its cursor survive.
//! - `from_iter` keeps the first value of a key, and `insert` keeps the last.
//!
//! Not exact:
//!
//! - **Each operation is atomic.** crossbeam's operations are linearizable too, but are made of many
//!   atomic steps, and the model does not explore interleavings inside one beyond those listed
//!   above. A search in crossbeam can also restart when it meets a node being removed, which the
//!   model has no need of.
//! - **`len` is exact.** crossbeam's is an approximation while the map is being modified.
//! - Insertion and lookup cost `O(n)` and `O(log n)`, not a skip list's expected `O(log n)`; the
//!   model is for tests, not for large maps.
//! - `Range` is `Send` and `Sync` when its parts are; crossbeam's is for any `K` and `V`.
//! - The `base` module and `SkipList`, crossbeam's guard-based API, are not modelled: naming them
//!   under the `shuttle` feature does not compile.
//!
//! # State per execution
//!
//! A map's entries belong to the running execution: they live in the execution's storage, and the
//! map holds only an identifier and the entries it was built with. So a map that outlives an
//! execution, such as one the test closure captured, starts every execution as it was built: empty,
//! or with the entries `from_iter` gave it outside of an execution. Entries still in a map when an
//! execution ends are dropped with the execution, unless an `Entry` still holds them.
//!
//! # Other differences from crossbeam-skiplist
//!
//! - The operations must run inside a Shuttle execution, and panic outside of one with Shuttle's
//!   "Are you accessing a Shuttle primitive outside of a Shuttle test?". Constructing a map,
//!   `from_iter`, `into_iter` (which then yields the entries the map was built with), `iter`,
//!   `range`, `Debug` and dropping work anywhere.
//! - `K`'s `Ord` impl runs while the map's state is borrowed, so it must not use the same map;
//!   `compare_fn`, `value_fn` and the destructors of keys and values may.
//! - A map that is leaked (with `mem::forget` or a reference cycle) still has its entries dropped
//!   when the execution ends, where crossbeam would leak them.
//! - The maps do not take part in Shuttle's annotated schedules as named resources.

// The scheduling points and the storage come from `shuttle-engine`. The dependency on `shuttle` only
// makes sure that the `shuttle` a test runs with is built on that engine (0.9.2 or later).
use shuttle as _;

pub mod map;
mod per_execution;
pub mod set;

#[doc(inline)]
pub use crate::{map::SkipMap, set::SkipSet};

/// The second half of this crate's keys in the execution storage, which tells them apart from the
/// keys of Shuttle's own primitives and of the other wrappers.
const STORAGE_TAG: usize = 0x6362_736c; // "cbsl"
