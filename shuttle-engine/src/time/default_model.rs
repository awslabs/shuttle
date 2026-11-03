use std::task::Waker;

use crate::time::WakerRegistered;

use super::{constant_stepped::ConstantSteppedTimeModel, Duration, Instant, TimeModel};

/// The time model of [`Runner::new`](crate::Runner::new), which keeps how Shuttle behaved before time models:
/// `thread::sleep` is a single scheduling point, after which the sleeping thread can run again right away, so tests
/// explore the same schedules whatever they sleep for. When the thread continues, the clock moves forward to the
/// deadline of its sleep. The clock doesn't move otherwise, and the timers of async sleeps, timeouts and intervals
/// fire when the clock reaches them or when no task can run, as under [`FrozenTimeModel`](super::frozen::FrozenTimeModel).
///
/// Clients can't name this type, so that it can change, or be renamed, later.
#[derive(Debug)]
pub(crate) struct DefaultTimeModel {
    inner: ConstantSteppedTimeModel,
}

impl DefaultTimeModel {
    pub(crate) fn new() -> Self {
        Self {
            inner: ConstantSteppedTimeModel::new(std::time::Duration::ZERO),
        }
    }
}

impl TimeModel for DefaultTimeModel {
    fn advance_to_next_deadline(&mut self) -> bool {
        self.inner.advance_to_next_deadline()
    }

    fn new_execution(&mut self) {
        self.inner.new_execution();
    }

    fn step(&mut self) {
        self.inner.step();
    }

    fn instant(&self) -> Instant {
        self.inner.instant()
    }

    fn pause(&mut self) {}

    fn resume(&mut self) {}

    fn advance(&mut self, duration: Duration) {
        self.inner.advance(duration);
    }

    fn register_sleep(&mut self, deadline: Instant, id: u64, waker: Waker) -> WakerRegistered {
        self.inner.register_sleep(deadline, id, waker)
    }

    fn unregister_sleep(&mut self, id: u64) {
        self.inner.unregister_sleep(id);
    }

    fn take_expired(&mut self) -> Vec<Waker> {
        self.inner.take_expired()
    }

    fn sleep_is_a_scheduling_point(&self) -> bool {
        true
    }
}
