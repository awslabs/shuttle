use std::task::Waker;

use crate::time::WakerRegistered;

use super::{constant_stepped::ConstantSteppedTimeModel, Duration, Instant, TimeModel};

/// A time model where time does not advance unless forced
#[derive(Debug, Clone)]
pub struct FrozenTimeModel {
    inner: ConstantSteppedTimeModel,
}

impl FrozenTimeModel {
    /// Create a new Frozen time model
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for FrozenTimeModel {
    fn default() -> Self {
        Self {
            inner: ConstantSteppedTimeModel::new(std::time::Duration::ZERO),
        }
    }
}

impl TimeModel for FrozenTimeModel {
    fn pause(&mut self) {
        self.inner.pause();
    }

    fn resume(&mut self) {
        self.inner.resume();
    }

    fn step(&mut self) {
        self.inner.step();
    }

    fn new_execution(&mut self) {
        self.inner.new_execution();
    }

    fn instant(&self) -> Instant {
        self.inner.instant()
    }

    fn advance_to_next_deadline(&mut self) -> bool {
        self.inner.advance_to_next_deadline()
    }

    fn advance(&mut self, dur: Duration) {
        self.inner.advance(dur);
    }

    fn register_sleep(&mut self, deadline: Instant, sleep_id: u64, waker: Waker) -> WakerRegistered {
        self.inner.register_sleep(deadline, sleep_id, waker)
    }

    fn unregister_sleep(&mut self, sleep_id: u64) {
        self.inner.unregister_sleep(sleep_id);
    }

    fn take_expired(&mut self) -> Vec<Waker> {
        self.inner.take_expired()
    }
}
