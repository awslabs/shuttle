//! Time
//!
//! Timing primitives allow Shuttle tests to interact with wall-clock time in a deterministic manner

use std::{
    cmp::{max, Reverse},
    collections::BinaryHeap,
    rc::Rc,
    time::Duration,
};

use tracing::debug;

use crate::{current::TaskId, runtime::execution::ExecutionState};

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
    ConstantStepped(ConstantTimeDistribution),
    /// Time is not advanced by Shuttle; `sleep` and related functions are a single scheduling
    /// point which may execute immediately or be delayed arbitrarily. This is the default time model.
    NoTime,
}

/// create a TimeModel corresponding to the config
pub fn from_config(config: TimeModelConfig) -> Box<dyn TimeModel> {
    match config {
        TimeModelConfig::ConstantStepped(distribution) => Box::new(ConstantSteppedModel::new(distribution)),
        TimeModelConfig::NoTime => unimplemented!(),
    }
}

/// A time model determines how Shuttle models wall-clock time
#[allow(unused)]
pub trait TimeModel {
    /// sleep
    fn sleep(&mut self, duration: Duration);
    /// wake the next sleeping task if all tasks are blocked
    fn wake_next(&mut self);
    /// reset
    fn reset(&mut self);
    /// step
    fn step(&mut self);
    /// instant
    fn instant(&self) -> Instant;
}

/// A time model where time advances by a constant amount for each step
#[allow(unused)]
#[derive(Clone, Debug)]
pub struct ConstantSteppedModel {
    distribution: ConstantTimeDistribution,
    current_step_size: Duration,
    current_time_elapsed: Duration,
    waiters: BinaryHeap<Reverse<(Duration, TaskId)>>,
}

impl ConstantSteppedModel {
    /// Create a ConstantSteppedModel
    pub fn new(distribution: ConstantTimeDistribution) -> Self {
        Self {
            distribution,
            current_step_size: distribution.sample(),
            current_time_elapsed: Duration::from_secs(0),
            waiters: BinaryHeap::new(),
        }
    }

    fn unblock_expired(&mut self, state: &mut ExecutionState) {
        while let Some(id) = self
            .waiters
            .peek()
            .map(|Reverse((t, id))| {
                if *t <= self.current_time_elapsed {
                    Some(*id)
                } else {
                    None
                }
            })
            .flatten()
        {
            _ = self.waiters.pop();
            state.get_mut(id).unblock();
        }
    }
}

impl TimeModel for ConstantSteppedModel {
    fn sleep(&mut self, duration: Duration) {
        debug!("sleep");
        // TODO: sleeping should not cause deadlocks
        // Hack 1: don't sleep if only one runnable task, should get some tests passing
        // Eventually, we need another TaskState which is Sleeping (rename sleep to something else)
        // Execution state can fast-forward the time to unblock sleepers if no tasks are runnable
        if duration < self.current_step_size {
            return;
        }
        let wake_time = self.current_time_elapsed + duration;
        let item = (wake_time, ExecutionState::with(|s| s.current().id()));
        self.waiters.push(Reverse(item));
        ExecutionState::with(|s| s.current_mut().block(false));
    }

    fn step(&mut self) {
        debug!("step");
        self.current_time_elapsed += self.current_step_size;
        ExecutionState::with(|s| self.unblock_expired(s));
    }

    fn reset(&mut self) {
        self.current_step_size = self.distribution.sample();
        self.current_time_elapsed = Duration::from_secs(0);
        self.waiters.clear();
    }

    fn instant(&self) -> Instant {
        Instant {
            simulated_time_since_start: self.current_time_elapsed,
        }
    }

    fn wake_next(&mut self) {
        debug!("wake next");
        if let Some(Reverse((time, _))) = self.waiters.peek() {
            self.current_time_elapsed = max(self.current_time_elapsed, *time);
        }

        ExecutionState::with(|s| self.unblock_expired(s));
    }
}

/// Simulated instant, measured from the start of the execution
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct Instant {
    simulated_time_since_start: Duration,
}

impl Instant {
    #[allow(unused)]
    fn duration_since(&self, earlier: Instant) -> Duration {
        self.simulated_time_since_start - earlier.simulated_time_since_start
    }

    #[allow(unused)]
    fn elapsed(&self) -> Duration {
        self.simulated_time_since_start
    }

    #[allow(unused)]
    fn now() -> Instant {
        let tm = ExecutionState::with(|s| Rc::clone(&s.time_model));
        let r = tm.borrow_mut().instant();
        r
    }
}
