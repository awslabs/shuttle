//! `Backoff`, whose `spin` and `snooze` yield to Shuttle's scheduler.

use std::cell::Cell;
use std::fmt;

const SPIN_LIMIT: u32 = 6;
const YIELD_LIMIT: u32 = 10;

/// Performs exponential backoff in spin loops.
///
/// # Differences from `crossbeam_utils::Backoff`
///
/// [`Backoff::spin`] and [`Backoff::snooze`] each take exactly one scheduling point, rather than
/// emitting a spin-loop hint 2^step times and later yielding the thread to the operating system.
/// One point is what a busy-wait needs under Shuttle: it lets the task the loop is waiting for run.
/// Taking 2^step of them would make the schedule's length depend on the backoff's step counter
/// without exploring anything new.
///
/// The step counter is crossbeam's, so [`Backoff::is_completed`] turns true after the same number
/// of calls, and a loop that gives up after it does the same thing in both modes.
///
/// A spin loop under Shuttle needs a fair scheduler. `shuttle::check_dfs` explores "run the
/// spinning task again" forever, so a test that spins has to use a random or PCT scheduler, as
/// Shuttle's own spin loops do.
pub struct Backoff {
    step: Cell<u32>,
}

impl Backoff {
    /// Creates a new `Backoff`.
    #[inline]
    pub fn new() -> Self {
        Backoff { step: Cell::new(0) }
    }

    /// Resets the `Backoff`.
    #[inline]
    pub fn reset(&self) {
        self.step.set(0);
    }

    /// Backs off in a lock-free loop.
    ///
    /// Takes one scheduling point, so that the task this loop is waiting for can run. Panics
    /// outside of a Shuttle test.
    #[inline]
    pub fn spin(&self) {
        shuttle::hint::spin_loop();

        if self.step.get() <= SPIN_LIMIT {
            self.step.set(self.step.get() + 1);
        }
    }

    /// Backs off in a blocking loop.
    ///
    /// Takes one scheduling point, so that the task this loop is waiting for can run. Panics
    /// outside of a Shuttle test.
    #[inline]
    pub fn snooze(&self) {
        shuttle::thread::yield_now();

        if self.step.get() <= YIELD_LIMIT {
            self.step.set(self.step.get() + 1);
        }
    }

    /// Returns `true` if exponential backoff has completed and blocking the thread is advised.
    #[inline]
    pub fn is_completed(&self) -> bool {
        self.step.get() > YIELD_LIMIT
    }
}

impl fmt::Debug for Backoff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Backoff")
            .field("step", &self.step)
            .field("is_completed", &self.is_completed())
            .finish()
    }
}

impl Default for Backoff {
    fn default() -> Backoff {
        Backoff::new()
    }
}
