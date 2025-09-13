//! Time
//!
//! Timing primitives allow Shuttle tests to interact with wall-clock time in a deterministic manner

pub mod constant_stepped;

/// A distribution of times which can be sampled
pub trait TimeDistribution<D> {
    /// Sample a duration from the given distribution
    fn sample(&self) -> D;
}

/// A time model determines how Shuttle models wall-clock time
pub trait TimeModel<I, D> {
    /// sleep
    fn sleep(&mut self, duration: D);
    /// wake the next sleeping task if all tasks are blocked
    fn wake_next(&mut self);
    /// reset
    fn reset(&mut self);
    /// step
    fn step(&mut self);
    /// instant
    fn instant(&self) -> I;
    /// pause
    fn pause(&mut self);
    /// resume
    fn resume(&mut self);
}

/// A Shuttle Duration
pub trait Duration: Clone + Copy {
    /// Create a duration from seconds
    fn from_secs(secs: u64) -> Self;
    /// Create a duration from milliseconds
    fn from_millis(millis: u64) -> Self;
    /// Get duration as seconds
    fn as_secs(&self) -> u64;
    /// Get duration as milliseconds
    fn as_millis(&self) -> u128;
}

/// A Shuttle Instant
pub trait Instant: Clone + Copy {
    /// The duration type associated with this instant
    type Duration: Duration;

    /// Returns an instant corresponding to "now"
    fn now() -> Self;
    /// Returns the amount of time elapsed since this instant
    fn elapsed(&self) -> Self::Duration;
    /// Returns the amount of time elapsed from another instant to this one
    fn duration_since(&self, earlier: Self) -> Self::Duration {
        self.checked_duration_since(earlier).unwrap()
    }
    /// Returns the amount of time elapsed from another instant to this one, or None if that instant is later
    fn checked_duration_since(&self, earlier: Self) -> Option<Self::Duration>;
    /// Add a duration to this instant
    fn checked_add(&self, duration: Self::Duration) -> Option<Self>;
    /// Subtract a duration from this instant
    fn checked_sub(&self, duration: Self::Duration) -> Option<Self>;
}
