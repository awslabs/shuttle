//! This crate provides a Shuttle-compatible implementation and wrapper for [`once_cell`] in order
//! to make it more ergonomic to run a codebase under Shuttle.
//!
//! [`once_cell`]: <https://crates.io/crates/once_cell>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "once_cell/shuttle",
//! ]
//!
//! [dependencies]
//! once_cell = { package = "shuttle-once_cell", version = "1" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible
//! `once_cell` can be done via the "shuttle" feature flag. Without it, this crate is once_cell.
//! With it, `once_cell::sync::{OnceCell, Lazy}` and `once_cell::race::{OnceNonZeroUsize, OnceBool,
//! OnceRef, OnceBox}` are Shuttle's models, with once_cell 1.21's API:
//!
//! * A `static` cell or `Lazy` is empty again at the start of every execution, and its
//!   initializer runs again, so an execution can't depend on the ones before it and a failing
//!   schedule replays.
//! * Initializing an empty cell is where tasks interleave: it takes two scheduling points. Reading
//!   a cell that has a value takes none.
//! * In `sync`, one initializer runs and the others wait for it; in `race`, every task that finds
//!   the cell empty runs its own initializer and the first value stored wins, as in once_cell.
//!
//! The methods that reach a stored value need `T: 'static` and a running Shuttle execution, a
//! `Lazy`'s initializer is an `F: Fn() -> T` rather than an `FnOnce`, and the value a task stores
//! is dropped at the end of the execution. `once_cell::unsync` is once_cell's own in both modes:
//! its cells can't be shared between tasks or put in a `static`, so there is nothing for Shuttle to
//! control. The [shuttle-once_cell-inner](https://crates.io/crates/shuttle-once_cell-inner) crate
//! documents all of this.
//!
//! By default the version of `once_cell` exported is the latest `1.x` version (the same as if you
//! had written `once_cell = "1"` in your Cargo.toml), at least 1.21. If you need to constrain the
//! version of `once_cell` that you depend on (eg. to pin the version), then this can be done by
//! adding an entry like:
//!
//! ```ignore
//! [dependencies]
//! once_cell-version-import-dont-use = { package = "once_cell", version = "=1.21.4" }
//! ```

// once_cell is `no_std` without its `std` feature, and so is this crate then, unless Shuttle's
// models, which need `std`, are in.
#![cfg_attr(not(feature = "shuttle"), no_std)]

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_once_cell_inner::*;
    } else {
        pub use once_cell_orig::*;
    }
}
