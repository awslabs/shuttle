//! Time
//!
//! Timing primitives allow Shuttle tests to interact with wall-clock time (Instant, Duration, Timeout, etc.) in a deterministic manner

use std::cmp::Ordering;
use std::future::Future;
use std::ops::{Add, AddAssign, Mul, Sub, SubAssign};

use std::{cell::RefCell, rc::Rc};

use std::pin::Pin;

use pin_project::pin_project;
use std::task::{Context, Poll, Waker};
use tracing::warn;

use crate::current::Labels;
use crate::runtime::execution::ExecutionState;

use crate::runtime::thread;
use crate::sync::time::frozen::FrozenTimeModel;

/// Constant stepped time model implementation
pub mod constant_stepped;
/// Frozen time model implementation
pub mod frozen;

/// Returns the current count of created timeout/sleep futures
pub fn timer_count() -> u64 {
    ExecutionState::with(|state| state.timer_id_counter)
}

fn increment_timer_counter() -> u64 {
    ExecutionState::with(|state| {
        state.timer_id_counter += 1;
        state.timer_id_counter
    })
}

/// A distribution of times which can be sampled
pub trait TimeDistribution<D> {
    /// Sample a duration from the given distribution
    fn sample(&self) -> D;
}

/// The trait implemented by each TimeModel
pub trait TimeModel: std::fmt::Debug {
    /// Wake the next sleeping task; returns true if there exists a task that was able to be woken.
    /// Called when all tasks are blocked to resolve timing based deadlocks (all unblocked tasks are sleeping).
    fn wake_next(&mut self) -> bool;
    /// Reset the TimeModel state for the next Shuttle iteration
    fn reset(&mut self);
    /// Callback after each scheduling step to allow the TimeModel to update itself
    fn step(&mut self);
    /// Used to create the TimeModel's Instant struct in functions like Instant::now()
    fn instant(&self) -> Instant;
    /// Pauses the TimeModel
    fn pause(&mut self);
    /// Resumes the TimeModel
    fn resume(&mut self);
    /// Manually advances the TimeModel's clock by a fixed amount
    fn advance(&mut self, duration: Duration);
    /// Callback for registering a sleep/timeout on the current task. It is up to the TimeModel
    /// implementation to determine when to wake the sleeping task. If no waker is provided, then
    /// the caller is polling whether it is currently expired but is not yet performing a blocking
    /// sleep.
    fn register_sleep(&mut self, deadline: Instant, id: u64, waker: Option<Waker>) -> bool;
    /// Downcast to Any for type casting / checking
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// Provides a reference to the current TimeModel for this execution
pub fn get_time_model() -> Rc<RefCell<dyn TimeModel>> {
    ExecutionState::with(|s| Rc::clone(&s.time_model))
}

/// Expire all current timeouts/sleeps requested by tasks whose tags match the
/// given predicate. May not be implemented by all TimeModels.
pub fn trigger_timeouts<F>(trigger: F)
where
    F: Fn(&Labels) -> bool + 'static,
{
    match get_time_model()
        .borrow_mut()
        .as_any_mut()
        .downcast_mut::<FrozenTimeModel>()
    {
        Some(model) => model.trigger_timeouts(trigger),
        None => warn!("trigger_timeouts is only available for the default FrozenTimeModel"),
    }
}

/// Remove all triggers to expire timeouts
pub fn clear_triggers() {
    match get_time_model()
        .borrow_mut()
        .as_any_mut()
        .downcast_mut::<FrozenTimeModel>()
    {
        Some(model) => model.clear_triggers(),
        None => warn!("trigger_timeouts is only available for the default FrozenTimeModel"),
    }
}

/// A Shuttle duration
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Duration {
    /// A concrete duration value
    Std(std::time::Duration),
}

impl Duration {
    /// The maximum duration.
    pub const MAX: Duration = Duration::Std(std::time::Duration::MAX);
    /// Zero duration.
    pub const ZERO: Duration = Duration::Std(std::time::Duration::ZERO);

    /// Creates a new Duration from the specified number of seconds.
    pub fn from_secs(secs: u64) -> Self {
        Duration::Std(std::time::Duration::from_secs(secs))
    }

    /// Creates a new Duration from the specified number of milliseconds.
    pub fn from_millis(millis: u64) -> Self {
        Duration::Std(std::time::Duration::from_millis(millis))
    }

    /// Creates a new Duration from the specified number of microseconds.
    pub fn from_micros(micros: u64) -> Self {
        Duration::Std(std::time::Duration::from_micros(micros))
    }

    /// Creates a new Duration from the specified number of nanoseconds.
    pub fn from_nanos(nanos: u64) -> Self {
        Duration::Std(std::time::Duration::from_nanos(nanos))
    }

    /// Returns the total number of nanoseconds contained by this Duration.
    pub fn as_nanos(&self) -> u128 {
        match self {
            Duration::Std(d) => d.as_nanos(),
        }
    }

    /// Returns the total number of microseconds contained by this Duration.
    pub fn as_micros(&self) -> u128 {
        self.as_nanos() / 1000
    }

    /// Returns the total number of milliseconds contained by this Duration.
    pub fn as_millis(&self) -> u128 {
        self.as_micros() / 1000
    }

    ///  Checked Duration addition. Computes self + other, returning None if overflow occurred.
    pub fn checked_add(&self, other: Duration) -> Option<Self> {
        match (self, other) {
            (Duration::Std(a), Duration::Std(b)) => a.checked_add(b).map(Duration::Std),
        }
    }

    ///  Checked Duration subtraction. Computes self - other, returning None if other is greater than self.
    pub fn checked_sub(&self, other: Duration) -> Option<Self> {
        match (self, other) {
            (Duration::Std(a), Duration::Std(b)) => a.checked_sub(b).map(Duration::Std),
        }
    }

    ///  Checked Duration multiplication. Computes self * other, returning None if overflow occurred.
    pub fn checked_mul(&self, b: u32) -> Option<Self> {
        match self {
            Duration::Std(a) => a.checked_mul(b).map(Duration::Std),
        }
    }

    pub(crate) fn unwrap_std(self) -> std::time::Duration {
        match self {
            Duration::Std(d) => d,
        }
    }
}

impl Ord for Duration {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Duration::Std(a), Duration::Std(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Duration {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Add for Duration {
    type Output = Duration;

    fn add(self, other: Self) -> Self {
        self.checked_add(other).unwrap()
    }
}

impl AddAssign for Duration {
    fn add_assign(&mut self, other: Self) {
        *self = self.checked_add(other).unwrap()
    }
}

impl SubAssign for Duration {
    fn sub_assign(&mut self, other: Self) {
        *self = self.checked_sub(other).unwrap()
    }
}

impl Mul<u32> for Duration {
    type Output = Duration;

    fn mul(self, other: u32) -> Self {
        self.checked_mul(other).unwrap()
    }
}

impl Mul<Duration> for u32 {
    type Output = Duration;

    fn mul(self, other: Duration) -> Duration {
        other.checked_mul(self).unwrap()
    }
}

/// A Shuttle Instant
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instant {
    /// Deterministically simulated clock time represented by a Duration from the start of the test
    Simulated(std::time::Duration),
}

impl Instant {
    /// Returns an instant corresponding to “now”.
    pub fn now() -> Self {
        get_time_model().borrow().instant()
    }

    /// Cast this instant to a simulated time represented by a std::time::Duration
    pub fn unwrap_simulated(self) -> std::time::Duration {
        match self {
            Instant::Simulated(d) => d,
        }
    }

    /// Returns the amount of time elapsed from another instant to this one, or None if that instant is later than this one.
    /// Due to monotonicity bugs, even under correct logical ordering of the passed Instants, this method can return None.
    pub fn checked_sub(&self, earlier: Instant) -> Option<Duration> {
        match (self, earlier) {
            (Instant::Simulated(a), Instant::Simulated(b)) => a.checked_sub(b).map(Duration::Std),
        }
    }

    /// Returns the amount of time elapsed from another instant to this one, or None if that instant is later than this one.
    /// Due to monotonicity bugs, even under correct logical ordering of the passed Instants, this method can return None.
    pub fn checked_duration_since(&self, earlier: Instant) -> Option<Duration> {
        self.checked_sub(earlier)
    }

    /// Returns Some(t) where t is the time self + duration if t can be represented as Instant (which means it’s inside the bounds
    /// of the underlying data structure), None otherwise.
    pub fn checked_add(&self, duration: Duration) -> Option<Self> {
        match (self, duration) {
            (Instant::Simulated(a), Duration::Std(b)) => a.checked_add(b).map(Instant::Simulated),
        }
    }

    /// Returns the amount of time elapsed since this instant.
    /// Previous Rust versions panicked when the current time was earlier than self. Currently this method returns a Duration of
    /// zero in that case. Future versions may reintroduce the panic.
    pub fn elapsed(&self) -> Duration {
        Instant::now()
            .checked_duration_since(*self)
            .unwrap_or(Duration::from_secs(0))
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;

    fn add(self, other: Duration) -> Instant {
        self.checked_add(other).unwrap()
    }
}

impl AddAssign<Duration> for Instant {
    fn add_assign(&mut self, other: Duration) {
        *self = self.checked_add(other).unwrap()
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;

    fn sub(self, earlier: Instant) -> Duration {
        self.checked_sub(earlier).unwrap()
    }
}

impl SubAssign<Duration> for Instant {
    fn sub_assign(&mut self, other: Duration) {
        *self = *self - other
    }
}

impl Sub<Duration> for Instant {
    type Output = Instant;

    fn sub(self, other: Duration) -> Instant {
        match (self, other) {
            (Instant::Simulated(i), Duration::Std(d)) => Instant::Simulated(i - d),
        }
    }
}

impl Ord for Instant {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Instant::Simulated(a), Instant::Simulated(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Instant {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Puts the current thread to sleep
/// Behavior of this function depends on the TimeModel provided to Shuttle
pub fn sleep(dur: Duration) {
    if dur == Duration::ZERO {
        thread::switch();
        return;
    }
    crate::future::block_on(async_sleep(dur));
}

/// Advances the current global time without putting the current thread to sleep
/// Behavior of this function depends on the TimeModel provided to Shuttle
pub fn advance(dur: Duration) {
    get_time_model().borrow_mut().advance(dur);
    thread::switch();
}

/// Returns a future which sleeps until the duration has elapsed
/// Behavior of this function depends on the TimeModel provided to Shuttle
pub fn async_sleep(dur: Duration) -> Sleep {
    let id = increment_timer_counter();
    Sleep {
        id,
        deadline: Instant::now().checked_add(dur).unwrap(),
    }
}

/// Returns a future which sleeps until the deadline is reached
/// Behavior of this function depends on the TimeModel provided to Shuttle
pub fn async_sleep_until(deadline: Instant) -> Sleep {
    let id = increment_timer_counter();
    Sleep { id, deadline }
}

/// Returns a struct which sleeps repeatedly at a fixed time intervals (a tokio::time::Interval)
/// Behavior of this function depends on the TimeModel provided to Shuttle
pub fn async_interval(dur: Duration) -> Interval {
    Interval {
        start: None,
        ticks: 0,
        current_interval: None,
        period: dur,
    }
}

/// Returns a struct which sleeps repeatedly at a fixed time intervals (a tokio::time::Interval)
/// This Interval starts at a fixed start time.
/// Behavior of this function depends on the TimeModel provided to Shuttle
pub fn async_interval_at(start: Instant, period: Duration) -> Interval {
    Interval {
        start: Some(start),
        ticks: 0,
        current_interval: None,
        period,
    }
}

/// A future which returns Poll::Pending until its deadline
#[pin_project]
#[derive(Debug)]
pub struct Sleep {
    id: u64,
    deadline: Instant,
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let is_expired = get_time_model()
            .borrow_mut()
            .register_sleep(self.deadline, self.id, None);
        if is_expired {
            Poll::Ready(())
        } else {
            let _ = get_time_model()
                .borrow_mut()
                .register_sleep(self.deadline, self.id, Some(cx.waker().clone()));
            Poll::Pending
        }
    }
}

impl Sleep {
    /// Returns the instant at which the future will complete.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Returns `true` if `Sleep` has elapsed.
    ///
    /// A `Sleep` instance is elapsed when the requested duration has elapsed.
    pub fn is_elapsed(&self) -> bool {
        self.deadline.checked_duration_since(Instant::now()).is_none()
    }

    /// Resets the `Sleep` instance to a new deadline.
    pub fn reset(self: Pin<&mut Self>, deadline: Instant) {
        let me = self.project();
        *me.deadline = deadline;
    }
}

/// Timeout a future
#[pin_project]
#[derive(Debug)]
pub struct Interval {
    start: Option<Instant>,
    ticks: u32,
    period: Duration,
    current_interval: Option<Pin<Box<Sleep>>>,
}

impl Interval {
    /// tick
    pub async fn tick(&mut self) -> Instant {
        let deadline = self.next_deadline();
        async_sleep_until(deadline).await;
        self.ticks += 1;
        deadline
    }

    fn next_deadline(&mut self) -> Instant {
        if let Some(start) = self.start {
            let mut total_duration = Duration::ZERO;
            total_duration += self.period * self.ticks;
            start.checked_add(total_duration).unwrap()
        } else {
            let now = Instant::now();
            self.start = Some(now);
            now
        }
    }

    /// poll tick
    pub fn poll_tick(&mut self, cx: &mut Context<'_>) -> Poll<Instant> {
        let deadline = self.next_deadline();
        if self.current_interval.is_none() {
            self.current_interval = Some(Box::pin(async_sleep_until(deadline)));
        }

        match self.current_interval.as_mut().unwrap().as_mut().poll(cx) {
            Poll::Ready(_) => {
                self.current_interval = None;
                Poll::Ready(deadline)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    /// reset
    pub fn reset(&mut self) {
        self.start = None;
        self.ticks = 0;
        if let Some(x) = self.current_interval.as_mut() {
            x.as_mut().reset(Instant::now());
        }
    }
}

/// Timeout a future
pub fn async_timeout<F>(d: Duration, f: F) -> Timeout<F>
where
    F: Future,
{
    let id = increment_timer_counter();
    Timeout {
        id,
        deadline: Instant::now() + d,
        future: f,
    }
}

/// Timeout a future
#[pin_project]
#[derive(Debug)]
pub struct Timeout<F>
where
    F: Future,
{
    id: u64,
    deadline: Instant,
    #[pin]
    future: F,
}

/// Elapsed time error variant
#[derive(Debug, Clone, Copy)]
pub struct Elapsed;

impl<F> Future for Timeout<F>
where
    F: Future,
{
    type Output = std::result::Result<F::Output, Elapsed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        let tm = get_time_model();
        let expired = tm.borrow_mut().register_sleep(*this.deadline, *this.id, None);
        if expired {
            return Poll::Ready(Err(Elapsed));
        }

        match this.future.poll(cx) {
            Poll::Pending => {
                let expired = tm
                    .borrow_mut()
                    .register_sleep(*this.deadline, *this.id, Some(cx.waker().clone()));
                if expired {
                    return Poll::Ready(Err(Elapsed));
                }
                Poll::Pending
            }
            Poll::Ready(x) => Poll::Ready(Ok(x)),
        }
    }
}
