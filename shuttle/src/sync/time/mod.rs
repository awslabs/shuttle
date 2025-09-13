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
