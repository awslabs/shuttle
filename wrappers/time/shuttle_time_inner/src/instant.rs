//! [`Instant`], with `std::time::Instant`'s API.

use crate::clock;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::time::Duration;

/// A measurement of a monotonically nondecreasing clock, as [`std::time::Instant`] is, except that
/// the clock is the running execution's rather than the operating system's. See the
/// [crate documentation](crate) for what it reads and what it does not say.
///
/// Unlike `std::time::Instant`, this type does not convert to or from `std::time::Instant`: see
/// [Conversions](crate#conversions).
///
/// `Debug` prints the duration since this type's own base, where `std::time::Instant` prints the
/// operating system's clock reading. Neither says anything a program should depend on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
// The field is the duration since the base, which is the same instant that the clock's start is for
// `SystemTime`. The base is far from zero so that subtracting a plausible duration from a reading
// does not overflow, as it does not with `std::time::Instant`.
pub struct Instant(Duration);

impl Instant {
    /// The instant `self` is the given duration after this type's base, for [`crate::timer_support`].
    pub(crate) fn from_since_base(since_base: Duration) -> Instant {
        Instant(since_base)
    }

    /// How long after this type's base `self` is, for [`crate::timer_support`].
    pub(crate) fn since_base(self) -> Duration {
        self.0
    }

    /// Returns an instant corresponding to "now".
    ///
    /// Reads the running execution's clock, and advances it by zero or one nanosecond. Panics
    /// outside of a Shuttle test.
    pub fn now() -> Instant {
        Instant(clock::read())
    }

    /// Returns the amount of time elapsed from another instant to this one, or zero duration if
    /// that instant is later than this one.
    pub fn duration_since(&self, earlier: Instant) -> Duration {
        self.checked_duration_since(earlier).unwrap_or_default()
    }

    /// Returns the amount of time elapsed from another instant to this one, or None if that instant
    /// is later than this one.
    pub fn checked_duration_since(&self, earlier: Instant) -> Option<Duration> {
        self.0.checked_sub(earlier.0)
    }

    /// Returns the amount of time elapsed from another instant to this one, or zero duration if
    /// that instant is later than this one.
    pub fn saturating_duration_since(&self, earlier: Instant) -> Duration {
        self.0.saturating_sub(earlier.0)
    }

    /// Returns the amount of time elapsed since this instant, by reading the clock again.
    pub fn elapsed(&self) -> Duration {
        Instant::now().duration_since(*self)
    }

    /// Returns `Some(t)` where `t` is the time `self + duration` if `t` can be represented as
    /// `Instant` (which means it's inside the bounds of the underlying data structure), None
    /// otherwise.
    pub fn checked_add(&self, duration: Duration) -> Option<Instant> {
        self.0.checked_add(duration).map(Instant)
    }

    /// Returns `Some(t)` where `t` is the time `self - duration` if `t` can be represented as
    /// `Instant` (which means it's inside the bounds of the underlying data structure), None
    /// otherwise.
    pub fn checked_sub(&self, duration: Duration) -> Option<Instant> {
        self.0.checked_sub(duration).map(Instant)
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;

    /// # Panics
    ///
    /// This function may panic if the resulting point in time cannot be represented by the
    /// underlying data structure. See [`Instant::checked_add`] for a version without panic.
    fn add(self, other: Duration) -> Instant {
        self.checked_add(other)
            .expect("overflow when adding duration to instant")
    }
}

impl AddAssign<Duration> for Instant {
    fn add_assign(&mut self, other: Duration) {
        *self = *self + other;
    }
}

impl Sub<Duration> for Instant {
    type Output = Instant;

    fn sub(self, other: Duration) -> Instant {
        self.checked_sub(other)
            .expect("overflow when subtracting duration from instant")
    }
}

impl SubAssign<Duration> for Instant {
    fn sub_assign(&mut self, other: Duration) {
        *self = *self - other;
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;

    /// Returns the amount of time elapsed from another instant to this one, or zero duration if
    /// that instant is later than this one.
    fn sub(self, other: Instant) -> Duration {
        self.duration_since(other)
    }
}
