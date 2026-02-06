//! Time
//!
//! Timing primitives allow Shuttle tests to interact with wall-clock time (Instant, Duration, Timeout, etc.) in a deterministic manner

use std::cmp::Ordering;
use std::future::Future;
#[cfg(feature = "advanced-time-models")]
use std::ops::Mul;
use std::ops::{Add, AddAssign, Sub, SubAssign};

use std::{cell::RefCell, rc::Rc};

use std::pin::Pin;

use pin_project::pin_project;
use std::task::{Context, Poll, Waker};
use tracing::warn;

use crate::runtime::execution::ExecutionState;

use crate::runtime::thread;

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
    fn new_execution(&mut self);

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
}

/// Provides a reference to the current TimeModel for this execution.
/// Uses `Rc::clone` so that ExecutionState isn't already borrowed when running most TimeModel methods
pub fn get_time_model() -> Rc<RefCell<dyn TimeModel>> {
    ExecutionState::with(|s| Rc::clone(&s.time_model))
}

#[cfg(feature = "advanced-time-models")]
mod advanced_duration {
    use super::*;

    /// A Shuttle duration
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
    pub enum Duration {
        /// A concrete duration value
        Std(std::time::Duration),
    }

    impl Default for Duration {
        fn default() -> Self {
            Duration::Std(std::time::Duration::ZERO)
        }
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

        /// Checked Duration division. Computes self / other, returning None if other is zero or overflow occurred.
        pub fn checked_div(&self, rhs: u32) -> Option<Self> {
            match self {
                Duration::Std(a) => a.checked_div(rhs).map(Duration::Std),
            }
        }

        /// Saturating Duration addition. Computes self + other, returning Duration::MAX if overflow occurred.
        pub fn saturating_add(&self, other: Duration) -> Self {
            match (self, other) {
                (Duration::Std(a), Duration::Std(b)) => Duration::Std(a.saturating_add(b)),
            }
        }

        /// Saturating Duration subtraction. Computes self - other, returning Duration::ZERO if other is greater than self.
        pub fn saturating_sub(&self, other: Duration) -> Self {
            match (self, other) {
                (Duration::Std(a), Duration::Std(b)) => Duration::Std(a.saturating_sub(b)),
            }
        }

        /// Saturating Duration multiplication. Computes self * other, returning Duration::MAX if overflow occurred.
        pub fn saturating_mul(&self, rhs: u32) -> Self {
            match self {
                Duration::Std(a) => Duration::Std(a.saturating_mul(rhs)),
            }
        }

        /// Multiplies Duration by f32.
        pub fn mul_f32(&self, rhs: f32) -> Self {
            match self {
                Duration::Std(a) => Duration::Std(a.mul_f32(rhs)),
            }
        }

        /// Multiplies Duration by f64.
        pub fn mul_f64(&self, rhs: f64) -> Self {
            match self {
                Duration::Std(a) => Duration::Std(a.mul_f64(rhs)),
            }
        }

        /// Divides Duration by f32.
        pub fn div_f32(&self, rhs: f32) -> Self {
            match self {
                Duration::Std(a) => Duration::Std(a.div_f32(rhs)),
            }
        }

        /// Divides Duration by f64.
        pub fn div_f64(&self, rhs: f64) -> Self {
            match self {
                Duration::Std(a) => Duration::Std(a.div_f64(rhs)),
            }
        }

        /// Returns the number of seconds contained by this Duration as f64.
        pub fn as_secs_f64(&self) -> f64 {
            match self {
                Duration::Std(d) => d.as_secs_f64(),
            }
        }

        /// Returns the number of seconds contained by this Duration as f32.
        pub fn as_secs_f32(&self) -> f32 {
            match self {
                Duration::Std(d) => d.as_secs_f32(),
            }
        }

        /// Returns the total number of whole seconds contained by this Duration.
        pub fn as_secs(&self) -> u64 {
            match self {
                Duration::Std(d) => d.as_secs(),
            }
        }

        /// Returns the fractional part of this Duration, in whole milliseconds.
        pub fn subsec_millis(&self) -> u32 {
            match self {
                Duration::Std(d) => d.subsec_millis(),
            }
        }

        /// Returns the fractional part of this Duration, in whole microseconds.
        pub fn subsec_micros(&self) -> u32 {
            match self {
                Duration::Std(d) => d.subsec_micros(),
            }
        }

        /// Returns the fractional part of this Duration, in nanoseconds.
        pub fn subsec_nanos(&self) -> u32 {
            match self {
                Duration::Std(d) => d.subsec_nanos(),
            }
        }

        /// Creates a new Duration from the specified number of whole seconds and additional nanoseconds.
        pub fn new(secs: u64, nanos: u32) -> Self {
            Duration::Std(std::time::Duration::new(secs, nanos))
        }

        /// Creates a new Duration from the specified number of seconds represented as f64.
        pub fn from_secs_f64(secs: f64) -> Self {
            Duration::Std(std::time::Duration::from_secs_f64(secs))
        }

        /// Creates a new Duration from the specified number of seconds represented as f32.
        pub fn from_secs_f32(secs: f32) -> Self {
            Duration::Std(std::time::Duration::from_secs_f32(secs))
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

    impl std::ops::Div<u32> for Duration {
        type Output = Duration;

        fn div(self, rhs: u32) -> Duration {
            self.checked_div(rhs).unwrap()
        }
    }

    impl std::ops::DivAssign<u32> for Duration {
        fn div_assign(&mut self, rhs: u32) {
            *self = self.checked_div(rhs).unwrap()
        }
    }

    impl From<std::time::Duration> for Duration {
        fn from(d: std::time::Duration) -> Self {
            Duration::Std(d)
        }
    }

    impl From<Duration> for std::time::Duration {
        fn from(d: Duration) -> Self {
            d.unwrap_std()
        }
    }

    impl std::ops::Sub for Duration {
        type Output = Duration;

        fn sub(self, other: Self) -> Self {
            self.checked_sub(other).unwrap()
        }
    }
}

#[cfg(feature = "advanced-time-models")]
pub use advanced_duration::Duration;

#[cfg(not(feature = "advanced-time-models"))]
pub use std::time::Duration;

/// A Shuttle Instant
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instant {
    /// Deterministically simulated clock time represented by a Duration from the start of the test
    Simulated(std::time::Duration),
}

#[cfg(feature = "advanced-time-models")]
impl Instant {
    /// Returns Some(t) where t is the time self - duration if t can be represented as Instant (which means it’s inside the bounds of the underlying data structure), None otherwise.
    pub fn checked_sub(&self, duration: Duration) -> Option<Instant> {
        match self {
            Instant::Simulated(a) => match duration {
                Duration::Std(b) => a.checked_sub(b).map(Instant::Simulated),
            },
        }
    }

    /// Returns the amount of time elapsed from another instant to this one, or None if that instant is later than this one.
    /// Due to monotonicity bugs, even under correct logical ordering of the passed Instants, this method can return None.
    pub fn checked_duration_since(&self, earlier: Instant) -> Option<Duration> {
        match (self, earlier) {
            (Instant::Simulated(a), Instant::Simulated(b)) => a.checked_sub(b).map(Duration::Std),
        }
    }

    /// Returns the amount of time elapsed from another instant to this one, or panics if that instant is later than this one.
    /// Due to monotonicity bugs, even under correct logical ordering of the passed Instants, this method can panic.
    pub fn saturating_duration_since(&self, earlier: Instant) -> Duration {
        {
            self.checked_duration_since(earlier).unwrap_or(Duration::ZERO)
        }
    }

    /// Returns Some(t) where t is the time self + duration if t can be represented as Instant (which means it's inside the bounds
    /// of the underlying data structure), None otherwise.
    pub fn checked_add(&self, duration: Duration) -> Option<Self> {
        match self {
            Instant::Simulated(a) => match duration {
                Duration::Std(b) => a.checked_add(b).map(Instant::Simulated),
            },
        }
    }
}

#[cfg(not(feature = "advanced-time-models"))]
impl Instant {
    /// Returns Some(t) where t is the time self - duration if t can be represented as Instant (which means it’s inside the bounds of the underlying data structure), None otherwise.
    pub fn checked_sub(&self, duration: Duration) -> Option<Instant> {
        match self {
            Instant::Simulated(a) => a.checked_sub(duration).map(Instant::Simulated),
        }
    }

    /// Returns the amount of time elapsed from another instant to this one, or None if that instant is later than this one.
    /// Due to monotonicity bugs, even under correct logical ordering of the passed Instants, this method can return None.
    pub fn checked_duration_since(&self, earlier: Instant) -> Option<Duration> {
        match (self, earlier) {
            (Instant::Simulated(a), Instant::Simulated(b)) => a.checked_sub(b),
        }
    }

    /// Returns the amount of time elapsed from another instant to this one, or panics if that instant is later than this one.
    /// Due to monotonicity bugs, even under correct logical ordering of the passed Instants, this method can panic.
    pub fn saturating_duration_since(&self, earlier: Instant) -> Duration {
        {
            self.checked_duration_since(earlier)
                .unwrap_or(std::time::Duration::ZERO)
        }
    }

    /// Returns Some(t) where t is the time self + duration if t can be represented as Instant (which means it's inside the bounds
    /// of the underlying data structure), None otherwise.
    pub fn checked_add(&self, duration: Duration) -> Option<Self> {
        match self {
            Instant::Simulated(a) => a.checked_add(duration).map(Instant::Simulated),
        }
    }
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

    /// Returns the amount of time elapsed from another instant to this one, or panics if that instant is later than this one.
    /// Due to monotonicity bugs, even under correct logical ordering of the passed Instants, this method can panic.
    pub fn duration_since(&self, earlier: Instant) -> Duration {
        self.checked_duration_since(earlier).unwrap()
    }

    /// Returns the amount of time elapsed since this instant.
    /// Previous Rust versions panicked when the current time was earlier than self. Currently this method returns a Duration of
    /// zero in that case. Future versions may reintroduce the panic.
    pub fn elapsed(&self) -> Duration {
        Instant::now()
            .checked_duration_since(*self)
            .unwrap_or(Duration::from_secs(0))
    }

    /// Returns t where t is the time self + duration if t can be represented as Instant otherwise it saturates to the maximum time value
    pub fn saturating_add(&self, duration: Duration) -> Self {
        match self {
            Instant::Simulated(_) => self.checked_add(duration).unwrap_or(Instant::Simulated(Duration::MAX)),
        }
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
        self.checked_duration_since(earlier).unwrap()
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
        self.checked_sub(other).unwrap()
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
    async_sleep_until(Instant::now().saturating_add(dur))
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
#[must_use = "futures do nothing unless you `.await` or poll them"]
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

/// Missed tick behavior for Interval
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissedTickBehavior {
    /// Ticks as fast as possible until caught up.
    Burst,
    /// Tick at multiples of period from when tick was called, rather than from start.
    Delay,
    /// Skips missed ticks and tick on the next multiple of period from start.
    Skip,
}

impl Interval {
    /// tick
    pub async fn tick(&mut self) -> Instant {
        let deadline = self.next_deadline();
        async_sleep_until(deadline).await;
        self.ticks += 1;
        deadline
    }

    /// period for an Interval
    pub fn period(&self) -> Duration {
        self.period
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

    /// Unimplemented
    pub fn set_missed_tick_behavior(&mut self, _behavior: MissedTickBehavior) {
        warn!("set missed tick behavior unimplemented: no effect!");
    }

    /// Unimplemented
    pub fn missed_tick_behavior(&mut self) -> MissedTickBehavior {
        warn!("set missed tick behavior unimplemented: no effect!");
        MissedTickBehavior::Burst
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
        deadline: Instant::now().saturating_add(d),
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
#[derive(Debug, PartialEq, Eq)]
pub struct Elapsed(());

impl std::fmt::Display for Elapsed {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        "deadline has elapsed".fmt(fmt)
    }
}

impl std::error::Error for Elapsed {}

impl From<Elapsed> for std::io::Error {
    fn from(_err: Elapsed) -> std::io::Error {
        std::io::ErrorKind::TimedOut.into()
    }
}

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
            return Poll::Ready(Err(Elapsed(())));
        }

        match this.future.poll(cx) {
            Poll::Pending => {
                let expired = tm
                    .borrow_mut()
                    .register_sleep(*this.deadline, *this.id, Some(cx.waker().clone()));
                if expired {
                    return Poll::Ready(Err(Elapsed(())));
                }
                Poll::Pending
            }
            Poll::Ready(x) => Poll::Ready(Ok(x)),
        }
    }
}
