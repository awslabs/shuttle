//! Time
//!
//! Timing primitives allow Shuttle tests to interact with wall-clock time (Instant, Duration, Timeout, etc.) in a deterministic manner

use std::cmp::Ordering;
use std::future::Future;
#[cfg(feature = "advanced-time-models")]
use std::ops::Mul;
use std::ops::{Add, AddAssign, Sub, SubAssign};

use std::{cell::RefCell, rc::Rc, sync::Arc};

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

/// A distribution of times which can be sampled
pub trait TimeDistribution<D> {
    /// Sample a duration from the given distribution
    fn sample(&self) -> D;
}

/// Whether or not a waker was registered on call to `register_sleep`
#[derive(Debug)]
pub enum WakerRegistered {
    /// Waker was registered (ie. caller can return `Poll::Pending`)
    Registered,
    /// Waker was not registered (ie. the time duration has elapsed)
    NotRegistered,
}

/// The trait implemented by each TimeModel
///
/// A model owns the clock and the registered timers, but never calls a timer's waker itself: Shuttle
/// collects the expired ones with [`TimeModel::take_expired`] after every clock change, and wakes them
/// once it has released its borrow of the model, because a waker may call back into Shuttle.
pub trait TimeModel: std::fmt::Debug {
    /// Moves the clock forward to the earliest registered deadline, if that is later than the current
    /// time. Returns false if no timer is registered. Called when no task is runnable, so that tasks
    /// waiting on a timer can make progress.
    fn advance_to_next_deadline(&mut self) -> bool;

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
    /// implementation to determine when to wake the sleeping task.
    ///
    /// Re-registering an id replaces its deadline and waker. If the deadline has elapsed, any
    /// existing registration for that id must be removed before returning `NotRegistered`.
    fn register_sleep(&mut self, deadline: Instant, id: u64, waker: Waker) -> WakerRegistered;

    /// Removes a timer and releases its waker, if it is registered.
    ///
    /// This is also called when timers are dropped during execution cleanup, so implementations
    /// must not require a current task. Removing an id that is no longer registered is a no-op.
    fn unregister_sleep(&mut self, id: u64);

    /// Removes every timer whose deadline is at or before the current time, and returns their wakers
    /// in the order they should be woken.
    fn take_expired(&mut self) -> Vec<Waker>;
}

/// Provides a reference to the current TimeModel for this execution.
/// Uses `Rc::clone` so that ExecutionState isn't already borrowed when running most TimeModel methods
#[track_caller]
pub fn get_time_model() -> Rc<RefCell<dyn TimeModel>> {
    ExecutionState::with(|s| Rc::clone(&s.time_model))
}

/// Advances the clock by one scheduling step and wakes the timers that expire. Does nothing while no
/// task is running: during cleanup, and while the scheduler wakes timers (whose wakers may reach a
/// scheduling point).
pub(crate) fn step_time_model() {
    let Some(time_model) = ExecutionState::with(|state| {
        (!state.in_cleanup() && !state.firing_timers).then(|| Rc::clone(&state.time_model))
    }) else {
        return;
    };
    let expired = {
        let mut time_model = time_model.borrow_mut();
        time_model.step();
        time_model.take_expired()
    };
    for waker in expired {
        waker.wake();
    }
}

/// Wakes the timers that have expired. The wakers run after the borrow of the time model is released,
/// as a waker may call back into Shuttle, the time model included.
pub(crate) fn wake_expired_timers(time_model: &RefCell<dyn TimeModel>) {
    let expired = time_model.borrow_mut().take_expired();
    for waker in expired {
        waker.wake();
    }
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
            // `Instant::Simulated` holds a `std::time::Duration`, which is not the same type as
            // `Duration` when `advanced-time-models` is on.
            Instant::Simulated(_) => self
                .checked_add(duration)
                .unwrap_or(Instant::Simulated(std::time::Duration::MAX)),
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
    // A destructor can sleep while its task unwinds, or while the execution is torn down. Nothing wakes a sleeper
    // once the execution has finished, so these sleeps are just a scheduling point, as every sleep was before time
    // models.
    if dur == Duration::ZERO || std::thread::panicking() || ExecutionState::with(|state| state.in_cleanup()) {
        thread::switch();
        return;
    }
    crate::future::block_on(async_sleep(dur));
}

/// Advances the current global time without putting the current thread to sleep
/// Behavior of this function depends on the TimeModel provided to Shuttle
pub fn advance(dur: Duration) {
    // The scheduling point comes first, as for every other visible operation, so that other tasks
    // can run before the clock jumps.
    thread::switch();
    let time_model = get_time_model();
    time_model.borrow_mut().advance(dur);
    wake_expired_timers(&time_model);
}

/// Returns a future which sleeps until the duration has elapsed
/// Behavior of this function depends on the TimeModel provided to Shuttle
pub fn async_sleep(dur: Duration) -> Sleep {
    async_sleep_until(Instant::now().saturating_add(dur))
}

/// Returns a future which sleeps until the deadline is reached
/// Behavior of this function depends on the TimeModel provided to Shuttle
pub fn async_sleep_until(deadline: Instant) -> Sleep {
    let (id, execution) = ExecutionState::with(|state| {
        state.timer_id_counter += 1;
        (state.timer_id_counter, Arc::clone(&state.timer_context))
    });
    Sleep {
        id,
        execution,
        deadline,
        registered: false,
        waker: None,
    }
}

/// Returns a struct which sleeps repeatedly at a fixed time intervals (a tokio::time::Interval)
/// Behavior of this function depends on the TimeModel provided to Shuttle
pub fn async_interval(dur: Duration) -> Interval {
    async_interval_at(Instant::now(), dur)
}

/// Returns a struct which sleeps repeatedly at a fixed time intervals (a tokio::time::Interval)
/// This Interval starts at a fixed start time.
/// Behavior of this function depends on the TimeModel provided to Shuttle
pub fn async_interval_at(start: Instant, period: Duration) -> Interval {
    Interval {
        period,
        sleep: Box::pin(async_sleep_until(start)),
    }
}

/// A future which returns Poll::Pending until its deadline
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Sleep {
    id: u64,
    execution: Arc<()>,
    deadline: Instant,
    registered: bool,
    // Keep the latest waker so reset can rearm the timer without another poll, even after it elapsed.
    waker: Option<Waker>,
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let result = this.register(cx.waker().clone());
        // Replace the saved waker after releasing the time-model borrow. In particular, dropping
        // the old waker's last reference may run user-defined destructors.
        if let Some(waker) = this.waker.as_mut() {
            waker.clone_from(cx.waker());
        } else {
            this.waker = Some(cx.waker().clone());
        }
        match result {
            WakerRegistered::Registered => Poll::Pending,
            WakerRegistered::NotRegistered => Poll::Ready(()),
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
        Instant::now() >= self.deadline
    }

    /// Resets the `Sleep` instance to a new deadline.
    pub fn reset(self: Pin<&mut Self>, deadline: Instant) {
        let this = self.get_mut();
        this.deadline = deadline;
        if let Some(waker) = this.waker.clone() {
            if matches!(this.register(waker), WakerRegistered::NotRegistered) {
                // A reset to an elapsed deadline must wake a previously pending waiter. Do this
                // after releasing the time-model borrow, as the waker may access the runtime.
                this.waker.as_ref().unwrap().wake_by_ref();
            }
        }
    }

    fn register(&mut self, waker: Waker) -> WakerRegistered {
        let time_model = ExecutionState::with(|state| {
            assert!(
                Arc::ptr_eq(&self.execution, &state.timer_context),
                "timer used outside the execution that created it"
            );
            Rc::clone(&state.time_model)
        });
        let result = time_model.borrow_mut().register_sleep(self.deadline, self.id, waker);
        self.registered = matches!(&result, WakerRegistered::Registered);
        result
    }

    fn cancel(&mut self) {
        if std::mem::take(&mut self.registered) {
            // A timer can outlive its execution. Never remove a later execution's registration
            // just because it reused the same timer id.
            if let Ok(Some(time_model)) = ExecutionState::try_with(|state| {
                Arc::ptr_eq(&self.execution, &state.timer_context).then(|| Rc::clone(&state.time_model))
            }) {
                time_model.borrow_mut().unregister_sleep(self.id);
            }
        }
        // Release the saved waker outside the time-model borrow, just as in poll.
        self.waker = None;
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// A timer that ticks repeatedly at a fixed period.
#[derive(Debug)]
pub struct Interval {
    period: Duration,
    sleep: Pin<Box<Sleep>>,
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
        std::future::poll_fn(|cx| self.poll_tick(cx)).await
    }

    /// period for an Interval
    pub fn period(&self) -> Duration {
        self.period
    }

    /// poll tick
    pub fn poll_tick(&mut self, cx: &mut Context<'_>) -> Poll<Instant> {
        let deadline = self.sleep.deadline();
        match self.sleep.as_mut().poll(cx) {
            Poll::Ready(()) => {
                self.sleep.as_mut().reset(deadline + self.period);
                Poll::Ready(deadline)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    /// Resets the interval to tick one period after the current time.
    pub fn reset(&mut self) {
        self.sleep.as_mut().reset(Instant::now() + self.period);
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
    Timeout {
        future: f,
        sleep: async_sleep(d),
    }
}

/// Timeout a future
#[pin_project]
#[derive(Debug)]
pub struct Timeout<F>
where
    F: Future,
{
    #[pin]
    future: F,
    #[pin]
    sleep: Sleep,
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
        let mut this = self.project();
        match this.future.poll(cx) {
            Poll::Ready(value) => {
                this.sleep.get_mut().cancel();
                Poll::Ready(Ok(value))
            }
            Poll::Pending => match this.sleep.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    this.sleep.get_mut().cancel();
                    Poll::Ready(Err(Elapsed(())))
                }
                Poll::Pending => Poll::Pending,
            },
        }
    }
}
