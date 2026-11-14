//! The channels that deliver instants: `after` and `at` (one message) and `tick` (a message per
//! period), on `shuttle-time`'s clock.
//!
//! A timer delivers once the clock has reached its delivery time. Checking that reads the clock
//! without advancing it (`timer_support::peek`), so it adds no step to the schedule. Nothing moves
//! the clock to a delivery time but a wait for it that times out, which happens only when no task
//! can run (see `crate::waiter::block`), so a timer never fires while some task can still make
//! progress, and timers fire in the order of their delivery times.

use crate::generation;
use shuttle_time_inner::time::{Duration, Instant};
use shuttle_time_inner::timer_support::peek;
use std::cell::RefCell;

/// The channel of `after` and `at`: one message, at `delivery_time`.
pub(crate) struct At {
    delivery_time: Instant,
    /// The execution the state belongs to (see `crate::generation`), and whether the message was
    /// received in it.
    received: RefCell<(u64, bool)>,
}

// Safety: as for `crate::chan::Chan`, only the tasks of one execution use a timer, one at a time.
unsafe impl Send for At {}
unsafe impl Sync for At {}

impl At {
    pub(crate) fn new(delivery_time: Instant) -> Self {
        Self {
            delivery_time,
            received: RefCell::new((generation::current(), false)),
        }
    }

    /// Whether this execution has received the message already. A timer from an earlier execution
    /// starts over.
    fn received(&self) -> bool {
        let current = generation::current();
        let mut received = self.received.borrow_mut();
        if received.0 != current {
            *received = (current, false);
        }
        received.1
    }

    pub(crate) fn is_ready(&self) -> bool {
        !self.received() && peek() >= self.delivery_time
    }

    /// Receives the message, if it is there.
    pub(crate) fn try_recv(&self) -> Option<Instant> {
        if !self.is_ready() {
            return None;
        }
        self.received.borrow_mut().1 = true;
        Some(self.delivery_time)
    }

    /// When the message arrives, unless it has been received already.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        (!self.received()).then_some(self.delivery_time)
    }
}

/// The channel of `tick`: a message every `period`, from `first`.
pub(crate) struct Tick {
    first: Instant,
    period: Duration,
    /// The execution the state belongs to (see `crate::generation`), and when the next message
    /// arrives in it.
    next: RefCell<(u64, Option<Instant>)>,
}

// Safety: as for `At`.
unsafe impl Send for Tick {}
unsafe impl Sync for Tick {}

impl Tick {
    pub(crate) fn new(first: Instant, period: Duration) -> Self {
        Self {
            first,
            period,
            next: RefCell::new((generation::current(), Some(first))),
        }
    }

    /// When the next message arrives, or `None` if the next delivery time is past what an `Instant`
    /// can hold. A ticker from an earlier execution starts over.
    fn next(&self) -> Option<Instant> {
        let current = generation::current();
        let mut next = self.next.borrow_mut();
        if next.0 != current {
            *next = (current, Some(self.first));
        }
        next.1
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.next().is_some_and(|next| peek() >= next)
    }

    /// Receives the message that is due, if one is. As crossbeam's, the next one is then due a period
    /// after the clock's reading, so that a late receiver does not get a burst of messages.
    pub(crate) fn try_recv(&self) -> Option<Instant> {
        let delivery_time = self.next().filter(|next| peek() >= *next)?;
        self.next.borrow_mut().1 = peek().checked_add(self.period);
        Some(delivery_time)
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.next()
    }
}
