use std::{
    cmp::{max, Reverse},
    collections::{BinaryHeap, HashMap},
    task::Waker,
};

use tracing::{trace, warn};

use crate::{current::TaskId, runtime::execution::ExecutionState};

use super::{Duration, Instant, TimeDistribution, TimeModel};

/// A time model where time advances by a constant amount for each scheduling step
#[derive(Clone, Debug)]
pub struct ConstantSteppedTimeModel {
    distribution: ConstantTimeDistribution,
    current_step_size: std::time::Duration,
    current_time_elapsed: std::time::Duration,
    waiters: BinaryHeap<Reverse<(std::time::Duration, TaskId, u64)>>,
    wakers: HashMap<u64, Waker>,
}

unsafe impl Send for ConstantSteppedTimeModel {}

impl ConstantSteppedTimeModel {
    /// Create a ConstantSteppedTimeModel
    pub fn new(step_size: std::time::Duration) -> Self {
        let distribution = ConstantTimeDistribution::new(step_size);
        Self {
            distribution,
            current_step_size: distribution.sample(),
            current_time_elapsed: std::time::Duration::from_secs(0),
            waiters: BinaryHeap::new(),
            wakers: HashMap::new(),
        }
    }

    fn unblock_expired(&mut self) {
        while let Some(waker_key) = self.waiters.peek().and_then(|Reverse((t, _, sleep_id))| {
            if *t <= self.current_time_elapsed {
                Some(*sleep_id)
            } else {
                None
            }
        }) {
            _ = self.waiters.pop();
            if let Some(waker) = self.wakers.remove(&waker_key) {
                waker.wake();
            }
        }
    }

    /// Get the currently sleeping tasks and deadlines. May contain duplicates
    pub fn get_waiters(&self) -> &[Reverse<(std::time::Duration, TaskId, u64)>] {
        self.waiters.as_slice()
    }

    /// Manually wake a task without affecting the global clock
    pub fn wake_frozen(&mut self, sleep_id: u64) {
        if let Some(waker) = self.wakers.remove(&sleep_id) {
            waker.wake();
        }
    }
}

impl TimeModel for ConstantSteppedTimeModel {
    fn pause(&mut self) {
        warn!("Pausing stepped model has no effect")
    }

    fn resume(&mut self) {
        warn!("Resuming stepped model has no effect")
    }

    fn step(&mut self) {
        self.current_time_elapsed += self.current_step_size;
        trace!("time step to {:?}", self.current_time_elapsed);
        self.unblock_expired();
    }

    fn new_execution(&mut self) {
        self.current_step_size = self.distribution.sample();
        self.current_time_elapsed = std::time::Duration::from_secs(0);
        self.waiters.clear();
        self.wakers.clear();
    }

    fn instant(&self) -> Instant {
        Instant::Simulated(self.current_time_elapsed)
    }

    fn wake_next(&mut self) -> bool {
        if self.waiters.is_empty() {
            return false;
        }
        if let Some(Reverse((time, _, _))) = self.waiters.peek() {
            self.current_time_elapsed = max(self.current_time_elapsed, *time);
        }
        self.unblock_expired();
        true
    }

    #[allow(clippy::useless_conversion)]
    fn advance(&mut self, dur: Duration) {
        self.current_time_elapsed += dur.into();
    }

    fn register_sleep(&mut self, deadline: Instant, sleep_id: u64, waker: Option<Waker>) -> bool {
        let deadline = deadline.unwrap_simulated();
        if deadline <= self.current_time_elapsed {
            return true;
        }

        if let Some(waker) = waker {
            let task_id = ExecutionState::with(|s| s.current().id());
            let item = (deadline, task_id, sleep_id);
            self.waiters.push(Reverse(item));
            self.wakers.insert(sleep_id, waker);
        }
        false
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// A constant distribution; each sample returns the same time
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct ConstantTimeDistribution {
    /// The time that will be returned on sampling
    pub time: std::time::Duration,
}

impl ConstantTimeDistribution {
    /// Create a new constant time distribution
    pub fn new(time: std::time::Duration) -> Self {
        Self { time }
    }
}

impl TimeDistribution<std::time::Duration> for ConstantTimeDistribution {
    fn sample(&self) -> std::time::Duration {
        self.time
    }
}
