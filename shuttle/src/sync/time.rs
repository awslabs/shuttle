//! Time
//!
//! Timing primitives allow Shuttle tests to interact with wall-clock time in a deterministic manner 

use std::time::Duration;

use crate::runtime::execution::ExecutionState;

/// A distribution of times which can be sampled
pub trait TimeDistribution {
    /// Sample a duration from the given distribution
    fn sample(&self) -> Duration;

}

/// A constant distrubution; each sample returns the same time
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct ConstantTimeDistribution {
    /// The time that will be returned on sampling
    pub time: Duration,
}

impl ConstantTimeDistribution {
    /// Create a new constant time distribution
    pub fn new(time: Duration) -> Self {
        Self { time }
    }
}

impl TimeDistribution for ConstantTimeDistribution {
    fn sample(&self) -> Duration {
        self.time
    }
}

/// The time model used by Shuttle primitives
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum TimeModelConfig {
    
    /// Each execution step of Shuttle advances the global time by a constant. That constant
    /// is sampled at the *beginning* of each Shuttle test iteration from a given distribution.
    ConstantStepped( ConstantTimeDistribution),
    /// Time is not advanced by Shuttle; `sleep` and related functions are a single scheduling 
    /// point which may execute immediately or be delayed arbitrarily. This is the default time model.
    NoTime,
}

pub fn from_config(config: TimeModelConfig) -> Box<dyn TimeModel> {
    match config {
        TimeModelConfig::ConstantStepped(distribution) => Box::new(ConstantSteppedModel::new(distribution)),
        TimeModelConfig::NoTime => unimplemented!(),
    }
}

#[allow(unused)]
pub(crate) trait TimeModel {
    fn sleep(&mut self, duration: Duration);
    fn reset(&mut self);
    fn step(&mut self);
    fn instant(&self) -> Instant;
}

#[allow(unused)]
pub struct ConstantSteppedModel {
    distribution: ConstantTimeDistribution,
    current_step_size: Duration,
    current_time_elapsed: Duration,
}

impl ConstantSteppedModel {
    #[allow(unused)]
    pub fn new(distribution: ConstantTimeDistribution) -> Self {
        Self {
            distribution,
            current_step_size: distribution.sample(),
            current_time_elapsed: Duration::from_secs(0),
        }
    }
}

impl TimeModel for ConstantSteppedModel {
    fn sleep(&mut self, duration: Duration) {
        self.current_time_elapsed += duration;
    }

    fn step(&mut self) {
        self.current_time_elapsed += self.current_step_size;
    }

    fn reset(&mut self) {
        self.current_step_size = self.distribution.sample();
        self.current_time_elapsed = Duration::from_secs(0);
    }

    fn instant(&self) -> Instant {
        Instant {
            simulated_time_since_start: self.current_time_elapsed,
        }
    }
}


pub struct Instant {
    simulated_time_since_start: Duration,
}

impl Instant {
    fn duration_since(&self, earlier: Instant) -> Duration {
        self.simulated_time_since_start - earlier.simulated_time_since_start
    }

    fn elapsed(&self) -> Duration {
        self.simulated_time_since_start
    }

    fn now() -> Instant {
        ExecutionState::with(|s| s.time_model.instant())
    }
}