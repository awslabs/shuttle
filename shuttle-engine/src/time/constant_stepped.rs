use std::{
    cmp::max,
    collections::{BTreeMap, HashMap},
    task::Waker,
};

use tracing::{trace, warn};

use crate::time::WakerRegistered;

use super::{Duration, Instant, TimeModel};

// Timers are ordered by deadline, and then by id, which is creation order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SleepKey {
    deadline: std::time::Duration,
    sleep_id: u64,
}

/// A time model where time advances by a constant amount for each scheduling step
#[derive(Clone, Debug)]
pub struct ConstantSteppedTimeModel {
    step_size: std::time::Duration,
    current_time_elapsed: std::time::Duration,
    waiters: BTreeMap<SleepKey, Waker>,
    // Index registrations by id so resetting or cancelling a timer also removes its old deadline.
    sleep_keys: HashMap<u64, SleepKey>,
}

impl ConstantSteppedTimeModel {
    /// Create a ConstantSteppedTimeModel
    pub fn new(step_size: std::time::Duration) -> Self {
        Self {
            step_size,
            current_time_elapsed: std::time::Duration::from_secs(0),
            waiters: BTreeMap::new(),
            sleep_keys: HashMap::new(),
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
        self.current_time_elapsed = self.current_time_elapsed.saturating_add(self.step_size);
        trace!("time step to {:?}", self.current_time_elapsed);
    }

    fn new_execution(&mut self) {
        self.current_time_elapsed = std::time::Duration::from_secs(0);
        self.waiters.clear();
        self.sleep_keys.clear();
    }

    fn instant(&self) -> Instant {
        Instant::Simulated(self.current_time_elapsed)
    }

    fn advance_to_next_deadline(&mut self) -> bool {
        if let Some((key, _)) = self.waiters.first_key_value() {
            self.current_time_elapsed = max(self.current_time_elapsed, key.deadline);
            true
        } else {
            false
        }
    }

    fn advance(&mut self, dur: Duration) {
        // `Duration` is `std::time::Duration` unless `advanced-time-models` is on, in which case it
        // is the enum that wraps it. The `From` conversion covers both, since the reflexive impl
        // handles the former; that is also why it is only a no-op in one of the two configurations.
        #[allow(clippy::useless_conversion)]
        let dur = std::time::Duration::from(dur);
        self.current_time_elapsed = self.current_time_elapsed.saturating_add(dur);
    }

    fn register_sleep(&mut self, deadline: Instant, sleep_id: u64, waker: Waker) -> WakerRegistered {
        let deadline = deadline.unwrap_simulated();
        if deadline <= self.current_time_elapsed {
            self.unregister_sleep(sleep_id);
            return WakerRegistered::NotRegistered;
        }

        let key = SleepKey { deadline, sleep_id };
        if let Some(previous) = self.sleep_keys.insert(sleep_id, key) {
            if previous != key {
                self.waiters.remove(&previous);
            }
        }
        self.waiters.insert(key, waker);

        WakerRegistered::Registered
    }

    fn unregister_sleep(&mut self, sleep_id: u64) {
        if let Some(key) = self.sleep_keys.remove(&sleep_id) {
            self.waiters.remove(&key);
        }
    }

    fn take_expired(&mut self) -> Vec<Waker> {
        let mut expired = Vec::new();
        while let Some(entry) = self.waiters.first_entry() {
            if entry.key().deadline > self.current_time_elapsed {
                break;
            }
            let (key, waker) = entry.remove_entry();
            self.sleep_keys.remove(&key.sleep_id);
            expired.push(waker);
        }
        expired
    }
}
