//! This crate provides a Shuttle-compatible implementation and wrapper for [`crossbeam-channel`]
//! in order to make it more ergonomic to run a codebase under Shuttle.
//!
//! [`crossbeam-channel`]: <https://crates.io/crates/crossbeam-channel>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "crossbeam-channel/shuttle",
//! ]
//!
//! [dependencies]
//! crossbeam-channel = { package = "shuttle-crossbeam-channel", version = "0.5" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible channels
//! can be done via the "shuttle" feature flag.
//!
//! # What changes under the feature
//!
//! Without the feature this crate is `crossbeam-channel`. With it, every item has crossbeam-channel
//! 0.5's API, but the channels are Shuttle's (see `shuttle-crossbeam-channel-inner` for the
//! details):
//!
//! - Sends, receives and selections are scheduling points, and a blocked one is a blocked Shuttle
//!   task, so Shuttle explores the orders in which the tasks use the channel.
//! - `select!` and `Select` pick among the ready operations with Shuttle's random number generator,
//!   so a failing schedule replays.
//! - **A timeout fires only when no task can run**, and moves `shuttle-time`'s clock to its
//!   deadline. The same holds for the timers of `after`, `at` and `tick`. A timeout therefore
//!   never fires in a schedule where the operation could still have completed, and a program that
//!   waits for one is not reported as deadlocked.
//! - A channel's `Instant` is `shuttle-time`'s, so code that passes deadlines takes its `Instant`
//!   from `shuttle_time::time`. The feature turns on `shuttle-time`'s `shuttle` feature too.
//! - The error types are crossbeam-channel's own in both modes.
//! - Every operation must run inside a Shuttle test, as the rest of Shuttle's primitives do.
//!
//! By default the version of `crossbeam-channel` exported is the latest `0.5` version (the same as
//! if you had written `crossbeam-channel = "0.5"` in your Cargo.toml). If you need to constrain the
//! version of `crossbeam-channel` that you depend on (eg. to pin the version), then this can be
//! done by adding an entry like:
//!
//! ```ignore
//! [dependencies]
//! crossbeam-channel-version-import-dont-use = { package = "crossbeam-channel", version = "=0.5.17" }
//! ```

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_crossbeam_channel_inner::*;
        // A dependency only to turn on its `shuttle` feature (see Cargo.toml).
        use shuttle_time as _;
    } else {
        // crossbeam-channel exports nothing without its `std` feature, so without it this glob
        // brings in nothing, which `unused_imports` reports.
        #[allow(unused_imports)]
        pub use crossbeam_channel_orig::*;
    }
}
