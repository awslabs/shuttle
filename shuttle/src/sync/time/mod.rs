//! Time
//!
//! Timing primitives allow Shuttle tests to interact with wall-clock time in a deterministic manner

use crate::sync::time::constant_stepped::ConstantSteppedTimeModel;

pub mod constant_stepped;

/// A distribution of times which can be sampled
pub trait TimeDistribution<D> {
    /// Sample a duration from the given distribution
    fn sample(&self) -> D;
}

/// A time model determines how Shuttle models wall-clock time
#[allow(unused)]
#[derive(Clone, Debug)]
pub enum TimeModel {
    /// A time model which increments the global time by a constant on each scheduling step
    ConstantSteppedTimeModel(ConstantSteppedTimeModel),
}

#[allow(unused)]
impl TimeModel {
    pub(crate) fn step(&mut self) {
        match self {
            TimeModel::ConstantSteppedTimeModel(model) => model.step(),
        }
    }

    pub(crate) fn reset(&mut self) {
        match self {
            TimeModel::ConstantSteppedTimeModel(model) => model.reset(),
        }
    }

    pub(crate) fn wake_next(&mut self) {
        match self {
            TimeModel::ConstantSteppedTimeModel(model) => model.wake_next(),
        }
    }
}

/// The trait implemented by each TimeModel
pub trait TimeModelShape {
    /// The associated Instant type for this TimeModel
    type TimeModelInstant: ShuttleModelInstant;
    /// The associated Duration type for this TimeModel
    type TimeModelDuration: ShuttleModelDuration;

    /// sleep
    fn sleep(&mut self, duration: Self::TimeModelDuration);
    /// wake the next sleeping task if all tasks are blocked
    fn wake_next(&mut self);
    /// reset
    fn reset(&mut self);
    /// step
    fn step(&mut self);
    /// instant
    fn instant(&self) -> Self::TimeModelInstant;
    /// pause
    fn pause(&mut self);
    /// resume
    fn resume(&mut self);
}

/// A Shuttle Duration
pub trait ShuttleModelDuration: Clone + Copy {
    /// The Instant type associated with this instant
    type DurationModelInstant: ShuttleModelInstant<InstantModelDuration = Self>;

    /// Create a duration from seconds
    fn from_secs(secs: u64) -> Self;
    /// Create a duration from milliseconds
    fn from_millis(millis: u64) -> Self;
    /// Get duration as seconds
    fn as_secs(&self) -> u64;
    /// Get duration as milliseconds
    fn as_millis(&self) -> u128;
    /// sleep
    fn sleep(&self);
}

/// A Shuttle Instant
pub trait ShuttleModelInstant: Clone + Copy {
    /// The duration type associated with this instant
    type InstantModelDuration: ShuttleModelDuration<DurationModelInstant = Self>;

    /// Returns an instant corresponding to "now"
    fn now() -> Self;
    /// Returns the amount of time elapsed since this instant
    fn elapsed(&self) -> Self::InstantModelDuration;
    /// Returns the amount of time elapsed from another instant to this one
    fn duration_since(&self, earlier: Self) -> Self::InstantModelDuration {
        self.checked_duration_since(earlier).unwrap()
    }
    /// Returns the amount of time elapsed from another instant to this one, or None if that instant is later
    fn checked_duration_since(&self, earlier: Self) -> Option<Self::InstantModelDuration>;
    /// Add a duration to this instant
    fn checked_add(&self, duration: Self::InstantModelDuration) -> Option<Self>;
    /// Subtract a duration from this instant
    fn checked_sub(&self, duration: Self::InstantModelDuration) -> Option<Self>;
}
