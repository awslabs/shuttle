//! [`SystemTime`], with `std::time::SystemTime`'s API.

use crate::clock;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::time::{Duration, SystemTimeError};

/// A measurement of the system clock, as [`std::time::SystemTime`] is, except that the clock is the
/// running execution's rather than the operating system's. See the [crate documentation](crate) for
/// what it reads and what it does not say.
///
/// The value is a `std::time::SystemTime`, and every operation other than [`SystemTime::now`]
/// delegates to it, so `Debug`, `Hash`, `Ord` and the arithmetic behave exactly as
/// `std::time::SystemTime`'s do, down to the panic messages. The type converts to and from
/// `std::time::SystemTime` with `From`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SystemTime(std::time::SystemTime);

/// An anchor in time which can be used to create new `SystemTime` instances.
///
/// This is `std::time::UNIX_EPOCH`, 1970-01-01T00:00:00Z, wrapped in this crate's type. The
/// `shuttle` feature does not move the Unix epoch; it moves what [`SystemTime::now`] reads.
pub const UNIX_EPOCH: SystemTime = SystemTime(std::time::UNIX_EPOCH);

impl SystemTime {
    /// An anchor in time which can be used to create new `SystemTime` instances.
    pub const UNIX_EPOCH: SystemTime = UNIX_EPOCH;

    /// Returns the system time corresponding to "now".
    ///
    /// Reads the running execution's clock, and advances it by zero or one nanosecond. In every
    /// execution the first reading is 2026-01-01T00:00:00Z. Panics outside of a Shuttle test.
    pub fn now() -> SystemTime {
        SystemTime(std::time::UNIX_EPOCH + clock::read())
    }

    /// Returns the amount of time elapsed from an earlier point in time.
    pub fn duration_since(&self, earlier: SystemTime) -> Result<Duration, SystemTimeError> {
        self.0.duration_since(earlier.0)
    }

    /// Returns the difference from this system time to the current clock time, by reading the clock
    /// again.
    pub fn elapsed(&self) -> Result<Duration, SystemTimeError> {
        SystemTime::now().duration_since(*self)
    }

    /// Returns `Some(t)` where `t` is the time `self + duration` if `t` can be represented as
    /// `SystemTime` (which means it's inside the bounds of the underlying data structure), None
    /// otherwise.
    pub fn checked_add(&self, duration: Duration) -> Option<SystemTime> {
        self.0.checked_add(duration).map(SystemTime)
    }

    /// Returns `Some(t)` where `t` is the time `self - duration` if `t` can be represented as
    /// `SystemTime` (which means it's inside the bounds of the underlying data structure), None
    /// otherwise.
    pub fn checked_sub(&self, duration: Duration) -> Option<SystemTime> {
        self.0.checked_sub(duration).map(SystemTime)
    }
}

impl Add<Duration> for SystemTime {
    type Output = SystemTime;

    /// # Panics
    ///
    /// This function may panic if the resulting point in time cannot be represented by the
    /// underlying data structure. See [`SystemTime::checked_add`] for a version without panic.
    fn add(self, dur: Duration) -> SystemTime {
        SystemTime(self.0 + dur)
    }
}

impl AddAssign<Duration> for SystemTime {
    fn add_assign(&mut self, other: Duration) {
        *self = *self + other;
    }
}

impl Sub<Duration> for SystemTime {
    type Output = SystemTime;

    fn sub(self, dur: Duration) -> SystemTime {
        SystemTime(self.0 - dur)
    }
}

impl SubAssign<Duration> for SystemTime {
    fn sub_assign(&mut self, other: Duration) {
        *self = *self - other;
    }
}

impl From<std::time::SystemTime> for SystemTime {
    fn from(time: std::time::SystemTime) -> Self {
        SystemTime(time)
    }
}

impl From<SystemTime> for std::time::SystemTime {
    fn from(time: SystemTime) -> Self {
        time.0
    }
}
