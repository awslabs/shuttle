//! Constant stepped time model

use std::{
    cmp::{max, Reverse},
    collections::BinaryHeap,
    ops::{Add, AddAssign, Sub, SubAssign},
    rc::Rc,
};

use tracing::{debug, warn};

use crate::{current::TaskId, runtime::execution::ExecutionState};

use super::{TimeDistribution, TimeModelShape};

/// A time model where time advances by a constant amount for each step
#[derive(Clone, Debug)]
pub struct ConstantSteppedTimeModel {
    distribution: ConstantTimeDistribution,
    current_step_size: Duration,
    current_time_elapsed: Duration,
    waiters: BinaryHeap<Reverse<(Duration, TaskId)>>,
}

unsafe impl Send for ConstantSteppedTimeModel {}

impl ConstantSteppedTimeModel {
    /// Create a ConstantSteppedTimeModel
    pub fn new(distribution: ConstantTimeDistribution) -> Self {
        Self {
            distribution,
            current_step_size: distribution.sample(),
            current_time_elapsed: Duration::from_secs(0),
            waiters: BinaryHeap::new(),
        }
    }

    fn unblock_expired(&mut self, state: &mut ExecutionState) {
        while let Some(id) = self.waiters.peek().and_then(|Reverse((t, id))| {
            if *t <= self.current_time_elapsed {
                Some(*id)
            } else {
                None
            }
        }) {
            _ = self.waiters.pop();
            state.get_mut(id).unblock();
        }
    }
}

impl TimeModelShape for ConstantSteppedTimeModel {
    type TimeModelInstant = Instant;
    type TimeModelDuration = Duration;

    fn pause(&mut self) {
        warn!("Pausing stepped model has no effect")
    }

    fn resume(&mut self) {
        warn!("Resuming stepped model has no effect")
    }

    fn sleep(&mut self, duration: Self::TimeModelDuration) {
        debug!("sleep");
        if duration == Duration::from_secs(0) {
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

    fn instant(&self) -> Self::TimeModelInstant {
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

impl TimeDistribution<Duration> for ConstantTimeDistribution {
    fn sample(&self) -> Duration {
        self.time
    }
}

/// A Shuttle Duration for stepped time
pub type Duration = std::time::Duration;

impl super::ShuttleModelDuration for Duration {
    type DurationModelInstant = Instant;

    fn sleep(self: &Duration) {
        let tm = ExecutionState::with(|s| Rc::clone(&s.time_model));
        let mut tm_borrow = tm.borrow_mut();
        match &mut *tm_borrow {
            super::TimeModel::ConstantSteppedTimeModel(model) => model.sleep(self.clone()),
        };
    }

    fn from_secs(secs: u64) -> Self {
        Duration::from_secs(secs)
    }

    fn from_millis(millis: u64) -> Self {
        Duration::from_millis(millis)
    }

    fn as_secs(&self) -> u64 {
        self.as_secs()
    }

    fn as_millis(&self) -> u128 {
        self.as_millis()
    }
}

/// Simulated instant, measured from the start of the execution
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant {
    simulated_time_since_start: Duration,
}

// SAFETY: Instant is never actually passed across threads, only across continuations.
// The Duration type is Send + Sync, so Instant can be as well.
unsafe impl Send for Instant {}
unsafe impl Sync for Instant {}

impl Instant {
    /// Converts a Tokio Instant to a std::Instant. This is a no-op for Shuttle Instants
    pub fn into_std(self) -> Instant {
        self
    }

    /// Converts a std::Instant to a Tokio Instant. This is a no-op for Shuttle Instants
    pub fn from_std(other: Instant) -> Instant {
        other
    }

    /// Returns the amount of time elapsed from another instant to this one
    pub fn duration_since(&self, earlier: Instant) -> Duration {
        self.simulated_time_since_start - earlier.simulated_time_since_start
    }

    /// Returns the amount of time elapsed from another instant to this one, or None if that instant is later than this one.
    pub fn checked_duration_since(&self, earlier: Instant) -> Option<Duration> {
        if self.simulated_time_since_start > earlier.simulated_time_since_start {
            Some(self.duration_since(earlier))
        } else {
            None
        }
    }

    /// Returns the amount of time elapsed from another instant to this one, or zero duration if that instant is later than this one.
    pub fn saturating_duration_since(&self, earlier: Instant) -> Duration {
        if self.simulated_time_since_start > earlier.simulated_time_since_start {
            self.duration_since(earlier)
        } else {
            Duration::from_nanos(0)
        }
    }

    /// Returns the amount of time elapsed since this instant was created
    pub fn elapsed(&self) -> Duration {
        Instant::now().duration_since(*self)
    }

    /// Returns an instant corresponding to "now"
    pub fn now() -> Instant {
        ExecutionState::with(|s| {
            let tm = Rc::clone(&s.time_model);
            let mut tm_borrow = tm.borrow_mut();
            match &mut *tm_borrow {
                super::TimeModel::ConstantSteppedTimeModel(model) => model.instant(),
            }
        })
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;

    fn add(self, other: Duration) -> Instant {
        Instant {
            simulated_time_since_start: self.simulated_time_since_start + other,
        }
    }
}

impl AddAssign<Duration> for Instant {
    fn add_assign(&mut self, other: Duration) {
        self.simulated_time_since_start += other;
    }
}

impl Sub<Duration> for Instant {
    type Output = Instant;

    fn sub(self, other: Duration) -> Instant {
        Instant {
            simulated_time_since_start: self.simulated_time_since_start - other,
        }
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;

    fn sub(self, other: Instant) -> Duration {
        self.saturating_duration_since(other)
    }
}

impl SubAssign<Duration> for Instant {
    fn sub_assign(&mut self, other: Duration) {
        self.simulated_time_since_start -= other;
    }
}

impl super::ShuttleModelInstant for Instant {
    type InstantModelDuration = Duration;

    fn now() -> Self {
        Instant::now()
    }

    fn elapsed(&self) -> Self::InstantModelDuration {
        self.elapsed()
    }

    fn checked_duration_since(&self, earlier: Self) -> Option<Self::InstantModelDuration> {
        self.checked_duration_since(earlier)
    }

    fn checked_add(&self, duration: Self::InstantModelDuration) -> Option<Self> {
        Some(*self + duration)
    }

    fn checked_sub(&self, duration: Self::InstantModelDuration) -> Option<Self> {
        Some(*self - duration)
    }
}
