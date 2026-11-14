//! This crate contains Shuttle's internal implementation of `std::time`'s clocks.
//! Do not depend on this crate directly. Use the [`shuttle-time`] crate, which conditionally
//! exposes this implementation with the `shuttle` feature or the original `std::time` without it.
//!
//! [`shuttle-time`]: <https://crates.io/crates/shuttle-time>
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//!
//! # The clock
//!
//! [`time::Instant::now`] and [`time::SystemTime::now`] read a counter of nanoseconds that belongs
//! to the running execution, not the operating system's clock:
//!
//! - Both read the same counter, so the two clocks agree with each other.
//! - Each call advances the counter, by a step that depends on whether the scheduler provides
//!   random data (see below). Either way a failing schedule replays with the same readings, and
//!   `shuttle::check_uncontrolled_nondeterminism` does not report the clock.
//! - The counter starts over in every execution, because its state lives in the storage of the
//!   execution. The first `SystemTime::now()` of an execution is 2026-01-01T00:00:00Z, and the
//!   first `Instant::now()` is the same number of nanoseconds after that type's own base.
//! - Reading the clock is not a scheduling point, as reading the operating system's clock is not
//!   one either. The draw from Shuttle's generator does not switch tasks.
//!
//! ## The step
//!
//! - **Where the scheduler provides random data**, as the random and PCT schedulers do, each call
//!   advances the counter by zero or one nanosecond. Which of the two is a random bool drawn from
//!   Shuttle (`shuttle::rand::try_thread_rng`, then `gen_bool(0.5)`), which records the draw in the
//!   schedule. Shuttle thereby explores both two equal readings and two different ones, and a
//!   schedule that depends on whether two readings differ replays.
//! - **Where it does not**, as under `shuttle::check_dfs`, each call advances the counter by
//!   exactly one nanosecond, without a draw. Every reading of an execution is then one nanosecond
//!   after the one before, so no two are equal. Reading the clock adds no step to the schedule and
//!   no choice to the search, so `check_dfs` explores the same schedules as it would without the
//!   readings.
//!
//! A replay of a schedule takes the steps its execution took, in either case. Destructors that run
//! while Shuttle tears an execution down get the zero-or-one step under any scheduler, from the
//! generator Shuttle keeps for teardown.
//!
//! **The timestamps track nothing real.** They say only how many times the execution has read the
//! clock before this reading, and so in which order the readings happened. Two readings a
//! nanosecond apart may be any distance apart in a real run, and two equal readings may not be.
//! Nothing here measures how long anything took.
//!
//! This is not virtual time. The crate models no timers of its own: the counter advances on a call
//! to `now()`, and when a timer of one of Shuttle's other wrappers fires (the `after`, `tick` and
//! timeouts of `shuttle-crossbeam-channel`, say), which moves it forward to the timer's deadline.
//! `shuttle::thread::sleep`, `tokio::time::sleep` and `tokio::time::timeout` do not advance it. A
//! loop that waits for a deadline to pass by reading the clock therefore spins until the execution
//! hits its step limit, both with this crate and with `std::time`.
//!
//! # Limitations
//!
//! - **Outside of a Shuttle test, `now()` panics**, as the rest of Shuttle does: "Are you accessing
//!   a Shuttle primitive outside of a Shuttle test?". Everything else works anywhere, including
//!   `UNIX_EPOCH`, the arithmetic and the comparisons. Code that reads the clock outside of a test
//!   has to read `std::time`'s clock directly.
//! - **How far back an `Instant` reaches.** `time::Instant::checked_sub`, and so `-`, refuse a
//!   duration longer than the reading's distance from this type's base, which is 56 years. On Linux
//!   `std::time::Instant` represents a time before the machine booted instead. Code that subtracts
//!   a plausible duration from a reading behaves the same either way.
//! - **Destructors that run after the clock's storage slot.** Shuttle drops the execution's storage
//!   slots in the order they were initialized, and statics last. The clock's slot is initialized at
//!   the first `now()`, so a destructor that runs after it and reads the clock panics with "global
//!   storage is never destructed". Found by reading the engine, not observed.
//!
//! # Conversions
//!
//! [`time::SystemTime`] converts to and from `std::time::SystemTime` with `From`, so code that
//! hands a system time to a crate built on `std::time` converts it with `.into()`. That call also
//! compiles without the `shuttle` feature, as a conversion from a type to itself, where
//! `clippy::useless_conversion` reports it.
//!
//! [`time::Instant`] has no such conversion, in either direction: `std::time::Instant` is opaque,
//! with no way to read its value and no way to build one at a chosen point without calling the real
//! clock. Code that has to hand a `std::time::Instant` to another crate therefore cannot take it
//! from this type.
//!
//! `Duration`, `SystemTimeError` and `TryFromFloatSecsError` are `std::time`'s own, in both modes.

/// `std::time`'s clocks, reading the running execution's clock instead of the operating system's.
///
/// `Instant` and `SystemTime` are this crate's own types, with `std::time`'s API. The rest of
/// `std::time` is `std::time`'s own, so `UNIX_EPOCH` still means 1970-01-01T00:00:00Z.
pub mod time {
    pub use crate::instant::Instant;
    pub use crate::system_time::{SystemTime, UNIX_EPOCH};
    pub use std::time::{Duration, SystemTimeError, TryFromFloatSecsError};
}

mod clock;
mod instant;
mod system_time;

/// What Shuttle's other wrappers need to model timers on this clock: a crossbeam channel's
/// `after`, `tick` and timeouts, for example. Not part of `std::time`'s API, and not re-exported
/// by `shuttle-time`.
///
/// A timer whose deadline the clock has not reached waits, without reading the clock, until it
/// fires: for the wrappers that this module serves, until no task can run (see
/// `shuttle_engine::runtime::task::Task::block_with_timeout`). The timer then moves the clock
/// forward to its deadline, so that the program never sees a timer fire before its deadline.
#[doc(hidden)]
pub mod timer_support {
    use crate::time::Instant;
    use shuttle_engine::runtime::execution::ExecutionState;
    use shuttle_engine::runtime::thread;
    use std::time::Duration;

    /// The clock's current reading, without advancing the clock and without a draw from Shuttle,
    /// unlike [`Instant::now`]. Panics outside of a Shuttle test.
    pub fn peek() -> Instant {
        Instant::from_since_base(crate::clock::peek())
    }

    /// Moves the clock forward to `deadline`, unless it has reached it already. Panics outside of a
    /// Shuttle test.
    pub fn advance_to(deadline: Instant) {
        crate::clock::advance_to(deadline.since_base());
    }

    /// How far `instant` is from the clock's origin, which is the same for every `Instant`. Timed
    /// waits order their deadlines by it.
    pub fn since_origin(instant: Instant) -> Duration {
        instant.since_base()
    }

    /// Blocks the current task until another task unblocks it, or until `deadline` passes. Returns
    /// whether the wait timed out.
    ///
    /// A wait with a deadline times out only when no task can run and the execution would otherwise
    /// deadlock (see `shuttle_engine::runtime::task::Task::block_with_timeout`), and the timeout then
    /// moves the clock forward to the deadline, so that the program never sees a timeout before its
    /// deadline. A wait without one blocks until another task unblocks it, as every other Shuttle
    /// primitive does.
    ///
    /// Panics outside of a Shuttle test.
    pub fn block_until(deadline: Option<Instant>) -> bool {
        ExecutionState::with(|state| match deadline {
            Some(deadline) => state.current_mut().block_with_timeout(since_origin(deadline)),
            None => state.current_mut().block(false),
        });
        thread::switch();
        let timed_out = ExecutionState::with(|state| state.current_mut().take_timed_out());
        if let (true, Some(deadline)) = (timed_out, deadline) {
            advance_to(deadline);
        }
        timed_out
    }
}
