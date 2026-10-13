//! Time
//!
//! Timing primitives allow Shuttle tests to interact with wall-clock time (Instant, Duration, Timeout, etc.) in a deterministic manner

use std::cmp::Ordering;
use std::future::Future;
use std::ops::{Add, AddAssign, Sub, SubAssign};

use std::{cell::RefCell, rc::Rc, sync::Arc};

use std::pin::Pin;

use pin_project::pin_project;
use std::task::{Context, Poll, Waker};

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

pub use std::time::Duration;

/// Where the clocks of Shuttle's time models start in every execution. The value is arbitrary but not zero, so that
/// code can compute instants in the past, such as `Instant::now() - ttl`, as it can with a real clock.
pub(crate) const SIMULATED_EPOCH: std::time::Duration = std::time::Duration::from_secs(86400 * 365);

/// A Shuttle Instant
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Instant {
    /// Deterministically simulated clock time, represented by a Duration since an arbitrary epoch. The clock starts
    /// at the same instant in every execution, which is not the epoch itself.
    Simulated(std::time::Duration),
}

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

    /// Returns the amount of time elapsed from another instant to this one, or zero duration if that instant is later than
    /// this one.
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

    /// Returns the amount of time elapsed from another instant to this one, or zero duration if that instant is later than
    /// this one, as `std::time::Instant::duration_since` does.
    pub fn duration_since(&self, earlier: Instant) -> Duration {
        self.saturating_duration_since(earlier)
    }

    /// Returns the amount of time elapsed since this instant, or zero duration if this instant is in the future.
    pub fn elapsed(&self) -> Duration {
        Instant::now().saturating_duration_since(*self)
    }

    /// An instant about 30 years from now, which stands in for deadlines too far away to represent. As in tokio, it is
    /// far enough away to never be reached by accident, and close enough that the clock can move there and on.
    pub(crate) fn far_future() -> Instant {
        const THIRTY_YEARS: std::time::Duration = std::time::Duration::from_secs(86400 * 365 * 30);
        Instant::Simulated(Instant::now().unwrap_simulated().saturating_add(THIRTY_YEARS))
    }

    /// The deadline `duration` after `self`, or `Instant::far_future()` if that is earlier or `self + duration` can't
    /// be represented. A timer that far out is never meant to fire, and capping it keeps the clock, which jumps to the
    /// next deadline when nothing else can run, away from the end of its range.
    pub(crate) fn deadline_after(self, duration: Duration) -> Instant {
        let far_future = Instant::far_future();
        self.checked_add(duration)
            .map_or(far_future, |deadline| deadline.min(far_future))
    }

    /// Returns t where t is the time self + duration if t can be represented as Instant otherwise it saturates to the maximum time value
    pub fn saturating_add(&self, duration: Duration) -> Self {
        self.checked_add(duration).unwrap_or(Instant::Simulated(Duration::MAX))
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;

    fn add(self, other: Duration) -> Instant {
        self.checked_add(other)
            .expect("overflow when adding duration to instant")
    }
}

impl AddAssign<Duration> for Instant {
    fn add_assign(&mut self, other: Duration) {
        *self = *self + other
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;

    /// Returns the amount of time elapsed from another instant to this one, or zero duration if that instant is later
    /// than this one, as for `std::time::Instant`.
    fn sub(self, earlier: Instant) -> Duration {
        self.duration_since(earlier)
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
        self.checked_sub(other)
            .expect("overflow when subtracting duration from instant")
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
    async_sleep_until(Instant::now().deadline_after(dur))
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
        armed: false,
        registered: false,
        waker: None,
    }
}

/// Returns an [`Interval`] that ticks every `period`, starting now, so that its first tick completes immediately. Like
/// tokio's `interval`.
/// Behavior of this function depends on the TimeModel provided to Shuttle
///
/// # Panics
///
/// Panics if `period` is zero.
pub fn async_interval(period: Duration) -> Interval {
    async_interval_at(Instant::now(), period)
}

/// Returns an [`Interval`] that ticks every `period`, with its first tick at `start`. Like tokio's `interval_at`.
/// Behavior of this function depends on the TimeModel provided to Shuttle
///
/// # Panics
///
/// Panics if `period` is zero.
pub fn async_interval_at(start: Instant, period: Duration) -> Interval {
    assert!(period > Duration::ZERO, "`period` must be non-zero.");
    Interval {
        period,
        missed_tick_behavior: MissedTickBehavior::default(),
        sleep: async_sleep_until(start),
    }
}

/// A future which returns Poll::Pending until its deadline
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Sleep {
    id: u64,
    execution: Arc<()>,
    deadline: Instant,
    // Whether the sleep has been polled or reset. tokio creates a sleep's timer only then, which `is_elapsed` reflects.
    armed: bool,
    registered: bool,
    // Keep the latest waker so reset can rearm the timer without another poll, even after it elapsed.
    waker: Option<Waker>,
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.armed = true;
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
    /// A `Sleep` instance is elapsed when the requested duration has elapsed, and it has been polled or reset. As with
    /// tokio's `Sleep`, a sleep that was never polled is not elapsed, even after its deadline.
    pub fn is_elapsed(&self) -> bool {
        self.armed && Instant::now() >= self.deadline
    }

    /// Resets the `Sleep` instance to a new deadline.
    pub fn reset(self: Pin<&mut Self>, deadline: Instant) {
        let this = self.get_mut();
        this.deadline = deadline;
        this.armed = true;
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

/// A timer that ticks repeatedly at a fixed period, like tokio's `Interval`. Created by [`async_interval`] and
/// [`async_interval_at`].
#[derive(Debug)]
pub struct Interval {
    period: Duration,
    missed_tick_behavior: MissedTickBehavior,
    // Due at the next tick.
    sleep: Sleep,
}

/// How an [`Interval`] schedules its next tick after it missed one, like tokio's `MissedTickBehavior`. A tick is missed
/// when it completes more than 5ms after it was due, as in tokio.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MissedTickBehavior {
    /// Ticks as fast as possible until caught up: the next tick is due one period after the missed one.
    #[default]
    Burst,
    /// Tick at multiples of period from when tick was called, rather than from start.
    Delay,
    /// Skips missed ticks and tick on the next multiple of period from start.
    Skip,
}

impl MissedTickBehavior {
    fn next_deadline(self, missed: Instant, now: Instant, period: Duration) -> Instant {
        match self {
            Self::Burst => missed.deadline_after(period),
            Self::Delay => now.deadline_after(period),
            Self::Skip => {
                let behind = u64::try_from((now - missed).as_nanos() % period.as_nanos())
                    .expect("too much time has elapsed since the interval was supposed to tick");
                now.deadline_after(period) - Duration::from_nanos(behind)
            }
        }
    }
}

impl Interval {
    /// Completes when the next tick is due, and returns the instant it was due at. The first tick completes
    /// immediately, unless the interval was created with a later start.
    pub async fn tick(&mut self) -> Instant {
        std::future::poll_fn(|cx| self.poll_tick(cx)).await
    }

    /// Returns the period of the interval.
    pub fn period(&self) -> Duration {
        self.period
    }

    /// Polls for the next tick, returning the instant it was due at once it is.
    pub fn poll_tick(&mut self, cx: &mut Context<'_>) -> Poll<Instant> {
        let deadline = self.sleep.deadline();
        if Pin::new(&mut self.sleep).poll(cx).is_pending() {
            return Poll::Pending;
        }
        let now = Instant::now();
        let next = if now > deadline.deadline_after(Duration::from_millis(5)) {
            self.missed_tick_behavior.next_deadline(deadline, now, self.period)
        } else {
            deadline.deadline_after(self.period)
        };
        Pin::new(&mut self.sleep).reset(next);
        Poll::Ready(deadline)
    }

    /// Resets the interval so that its next tick is due one period from now.
    pub fn reset(&mut self) {
        Pin::new(&mut self.sleep).reset(Instant::now().deadline_after(self.period));
    }

    /// Sets how the interval schedules its next tick after it missed one.
    pub fn set_missed_tick_behavior(&mut self, behavior: MissedTickBehavior) {
        self.missed_tick_behavior = behavior;
    }

    /// Returns how the interval schedules its next tick after it missed one.
    pub fn missed_tick_behavior(&self) -> MissedTickBehavior {
        self.missed_tick_behavior
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
#[must_use = "futures do nothing unless you `.await` or poll them"]
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
