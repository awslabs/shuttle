//! This crate provides a Shuttle-compatible wrapper for [`std::time`] in order to make it more
//! ergonomic to run a codebase under Shuttle.
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "shuttle-time/shuttle",
//! ]
//!
//! [dependencies]
//! shuttle-time = "VERSION_NUMBER"
//! ```
//!
//! Then swap usages of `std::time` with `shuttle_time::time`. The code behaves as before when the
//! `shuttle` feature flag is not provided, and reads a clock Shuttle controls when it is.
//!
//! # What the `shuttle` feature changes
//!
//! Without the feature this module is `std::time`. With it, [`time::Instant`] and
//! [`time::SystemTime`] are this crate's own types with `std::time`'s API, and their `now()` reads a
//! counter that belongs to the running execution instead of the operating system's clock. The
//! counter starts over in every execution, so a failing schedule replays and
//! [`shuttle::check_uncontrolled_nondeterminism`] does not report the clock as uncontrolled
//! nondeterminism.
//!
//! `Duration` and `SystemTimeError` are `std::time`'s in both modes.
//!
//! **The timestamps track nothing real.** They say only how many times the execution has read the
//! clock, and in which order. See [`time`] for the details of the clock, and for the two
//! limitations the feature brings: `now()` panics outside of a Shuttle test, and a
//! `time::Instant` does not convert to or from a `std::time::Instant`.
//!
//! This crate does not model timers. Under Shuttle, sleeps and timeouts return immediately and do
//! not advance this clock; a loop that waits for a deadline to pass therefore spins until the
//! execution hits its step limit, both with and without this crate.
//!
//! [`shuttle::check_uncontrolled_nondeterminism`]: <https://docs.rs/shuttle/latest/shuttle/fn.check_uncontrolled_nondeterminism.html>

/// `std::time`, or Shuttle's version of it under the `shuttle` feature.
///
/// Under the feature, see [`shuttle_time_inner`](https://docs.rs/shuttle-time-inner) for what each
/// item is. In short:
///
/// - `Instant::now()` and `SystemTime::now()` read a counter of nanoseconds that belongs to the
///   running execution. Each call advances it by zero or one nanosecond, chosen by a random bool
///   drawn from Shuttle, which records the draw in the schedule, or, where the scheduler provides no
///   random data (as under `shuttle::check_dfs`), by exactly one nanosecond. Both types read the
///   same counter, so the two clocks agree with each other.
/// - The counter starts over in every execution: a `SystemTime` at 2026-01-01T00:00:00Z, and an
///   `Instant` the same number of nanoseconds after its own (unobservable) base.
/// - Reading the clock is not a scheduling point, as reading the real clock is not one either.
/// - Both `now()`s panic outside of a Shuttle test, as the rest of Shuttle does.
/// - `Duration`, `SystemTimeError` and `TryFromFloatSecsError` are `std::time`'s in both modes, and
///   so `UNIX_EPOCH` still means 1970-01-01T00:00:00Z.
/// - A `SystemTime` converts to and from `std::time::SystemTime` with `From`. An `Instant` does
///   not: `std::time::Instant` is opaque, and neither direction can be written.
pub mod time {
    cfg_if::cfg_if! {
        if #[cfg(feature = "shuttle")] {
            pub use shuttle_time_inner::time::*;
        } else {
            pub use std::time::*;
        }
    }
}
