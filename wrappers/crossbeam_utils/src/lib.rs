//! This crate provides a Shuttle-compatible implementation and wrapper for [`crossbeam-utils`] in
//! order to make it more ergonomic to run a codebase under Shuttle.
//!
//! [`crossbeam-utils`]: <https://crates.io/crates/crossbeam-utils>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "crossbeam-utils/shuttle",
//! ]
//!
//! [dependencies]
//! crossbeam-utils = { package = "shuttle-crossbeam-utils", version = "0.8" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible primitives
//! can be done via the "shuttle" feature flag.
//!
//! # What changes under the feature
//!
//! Without the feature this crate is `crossbeam-utils`. With it, every item has crossbeam-utils
//! 0.8's API, but the primitives are Shuttle's (see `shuttle-crossbeam-utils-inner` for the
//! details):
//!
//! - `Backoff::spin` and `snooze` take a scheduling point, so a busy-wait lets the task it waits
//!   for run.
//! - `atomic::AtomicCell`'s operations are atomic steps of the execution, with Shuttle's causality.
//! - `sync::ShardedLock` is Shuttle's `RwLock`, `sync::WaitGroup` uses Shuttle's `Mutex` and
//!   `Condvar`, and `sync::Parker` blocks a Shuttle task, so Shuttle explores the orders in which
//!   the tasks reach them and reports a deadlock instead of hanging.
//! - `thread::scope`'s threads are Shuttle tasks, on `shuttle::thread::scope`.
//! - **`Parker::park_timeout` times out only when no task can run**, and then moves
//!   `shuttle-time`'s clock to its deadline, so a park never returns early in a schedule where the
//!   unparking task could still have run. `park_deadline` takes `shuttle_time::time::Instant`.
//! - `CachePadded` is crossbeam-utils's own type in both modes: it aligns a value and has nothing
//!   to schedule.
//! - Every operation must run inside a Shuttle test, as the rest of Shuttle's primitives do.
//!
//! By default the version of `crossbeam-utils` exported is the latest `0.8` version (the same as if
//! you had written `crossbeam-utils = "0.8"` in your Cargo.toml). If you need to constrain the
//! version of `crossbeam-utils` that you depend on (eg. to pin the version), then this can be done
//! by adding an entry like:
//!
//! ```ignore
//! [dependencies]
//! crossbeam-utils-version-import-dont-use = { package = "crossbeam-utils", version = "=0.8.23" }
//! ```

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_crossbeam_utils_inner::*;
        // A dependency only to turn on its `shuttle` feature (see Cargo.toml).
        use shuttle_time as _;
    } else {
        pub use crossbeam_utils_orig::*;
    }
}
