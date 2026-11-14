//! This is the "inner" crate implementing [`once_cell`] support for [`Shuttle`].
//! This crate should not be depended on directly, the intended way to use this crate is via
//! the `shuttle-once_cell` crate and feature flag `shuttle`.
//!
//! This crate has once_cell 1.21's API. Its [`sync`] and [`race`] modules are Shuttle's models of
//! once_cell's, with the same types, methods and trait implementations behind the same features, so
//! code written against once_cell compiles and behaves the same, except where Shuttle has to differ
//! to explore it (listed below). Its [`unsync`] module is once_cell's own.
//!
//! # What changes under Shuttle
//!
//! * **Every execution starts with empty cells.** A cell keeps the value that a task stores in it
//!   in the storage of the running execution, as Shuttle's `std::sync::OnceLock` model does, which
//!   these models are built on. So a `static` cell or `Lazy` is empty again at the start of every
//!   execution, and its initializer runs again, as with `shuttle::lazy_static!`. A value that
//!   carried over would make an execution depend on the ones before it, and a failing schedule
//!   could then not be replayed. A cell created with a value (`OnceCell::with_value`,
//!   `OnceBox::with_value`, `From`, `Clone`) holds that value itself, and so has it in every
//!   execution.
//! * **Initialization is where tasks interleave.** Storing a value in an empty cell takes two
//!   scheduling points, as `OnceLock::get_or_init` does: before the initialization check, where
//!   racing initializers contend, and before the value is released to them. Once a cell has a
//!   value, reading it, however it is spelled (`get`, `get_or_init`, `set`, `force`, a deref,
//!   `wait`), takes none, and nor does anything that holds a `&mut` borrow of the cell. A task that
//!   reads a value takes causality from the task that stored it.
//! * **`sync` blocks, `race` does not.** In [`sync`], one initializer runs and the others wait for
//!   it, as in once_cell. In [`race`], every task that finds the cell empty runs its own
//!   initializer, outside of any lock, and the first to store its value wins, as in once_cell; so
//!   Shuttle explores the schedules in which several initializers run as well as those in which
//!   one does.
//! * **Blocking forever is a deadlock.** A reentrant `sync` initialization, which deadlocks in
//!   once_cell, and a `sync::OnceCell::wait` that no task will satisfy fail the test with Shuttle's
//!   deadlock error.
//!
//! # Differences from once_cell
//!
//! These follow from where the values live, and are the same as for Shuttle's
//! `std::sync::OnceLock` and `std::sync::LazyLock`:
//!
//! * The methods that reach a value that a task stores require `T: 'static`, which the execution's
//!   storage requires. The structs do not, so a field of type `OnceCell<&'a str>` still compiles;
//!   its methods do not exist. `race::OnceRef<'a, T>` has no such requirement.
//! * They have to run inside a Shuttle execution, including `Debug` and `PartialEq`, which read the
//!   value. A cell created with a value can be read anywhere.
//! * A value that a task stores is dropped when the execution ends, not when the cell is dropped.
//! * A `sync::Lazy` runs its initializer once per execution, so it takes an `F: Fn() -> T` where
//!   once_cell takes an `F: FnOnce() -> T`. Every closure that does not consume what it captures
//!   is one, and so is the default `fn() -> T`. For the same reason a `Lazy` is `Sync` when its
//!   `F` is `Sync`, where once_cell's needs `F: Send`: the model calls the initializer through a
//!   shared reference, and executions of tests that run in parallel run on different threads.
//! * `get_unchecked` checks that the cell has a value, and panics if it has none, rather than
//!   leave that to the caller.
//! * The `Debug` output of the `race` types shows the address of the value, as once_cell's does,
//!   and the address differs between the two.
//!
//! The features that choose how once_cell implements its cells (`parking_lot`,
//! `critical-section`, `portable-atomic` and `atomic-polyfill`) make no difference to the models,
//! which replace that implementation; they still reach once_cell, for [`unsync`]. Under
//! `critical-section` without `std`, a reentrant initialization panics in once_cell, and fails with
//! Shuttle's deadlock error here.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//!
//! [`once_cell`]: <https://crates.io/crates/once_cell>

#[cfg(feature = "race")]
pub mod race;
#[cfg(any(feature = "std", feature = "critical-section"))]
pub mod sync;

/// once_cell's single-threaded cells, which are once_cell's own under Shuttle too.
///
/// They need no model. Their types are `!Sync`, so tasks cannot share one, which leaves no
/// interleaving to explore, and they cannot be the type of a `static`, so no cell outlives the
/// execution that created it: an execution can only reach one that it created itself, or one that
/// a primitive which outlives executions (a `std::thread_local!`, say) hands it, and that
/// primitive is what then needs a Shuttle model.
pub mod unsync {
    pub use once_cell_orig::unsync::*;
}
