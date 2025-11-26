//! Shuttle stubs for tokio time utilities

use pin_project::{pin_project, pinned_drop};
use shuttle::current::{get_current_task, with_labels_for_task, Labels, TaskId};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};
pub use std::time::Duration;
pub use tokio::time::Instant;
use tracing::trace;

pub fn pause() {}

// We don't attempt to model time directly.  Instead, we provide hooks for
// test frameworks to selectively force timeouts.  This is achieved by
// remembering the tag (if any) of the task that requested a timeout.

// The following global table records (1) the set of all currently active
// timeouts, as well as (2) the set of all registered expiry triggers.

thread_local! {
    static TIMEOUT_TABLE: Map = Map::default();
}

#[derive(Default)]
struct Map {
    inner: Mutex<InnerMap>,
}

#[derive(Default)]
struct InnerMap {
    // Timeouts that are currently active.  Each timeout is assigned a unique slot, which
    // can be used to look up the timeout in the BTreeMap.
    entries: BTreeMap<usize, TimeoutEntry>,
    // The next available slot
    next_slot: usize,
    // The set of triggers that have already expired
    #[allow(clippy::type_complexity)]
    triggers: Vec<Box<dyn Fn(&Labels) -> bool>>,
}

#[derive(Debug)]
struct TimeoutEntry {
    task_id: TaskId,
    state: TimeoutState,
}

#[derive(Debug)]
enum TimeoutState {
    Waiting(Option<Waker>),
    Expired,
}

// SAFETY: We are running single-threaded, and all accesses to the InnerMap are guarded
// by a std::sync::Mutex.
unsafe impl Send for Map {}
unsafe impl Sync for Map {}

/// Expire all current and future timeouts requested by tasks whose tags match the
/// given predicate.
pub fn trigger_timeouts<F>(trigger: F)
where
    F: Fn(&Labels) -> bool + 'static,
{
    let wakers = TIMEOUT_TABLE.with(|table| {
        let mut map = table.inner.lock().unwrap();
        let mut wakers = vec![];
        for TimeoutEntry { task_id, state } in map.entries.values_mut() {
            let task_name = format!("{task_id:?}");
            with_labels_for_task(*task_id, |labels| {
                if trigger(labels) {
                    trace!("triggering timeout for task {}", task_name);
                    match state {
                        TimeoutState::Expired => {}
                        TimeoutState::Waiting(waker) => {
                            if let Some(waker) = waker {
                                wakers.push(waker.clone());
                            }
                        }
                    }
                    *state = TimeoutState::Expired;
                }
            });
        }
        map.triggers.push(Box::new(trigger));
        drop(map);
        wakers
    });
    for waker in wakers {
        waker.wake();
    }
}

/// Clear all timeout triggers
pub fn clear_triggers() {
    TIMEOUT_TABLE.with(|table| {
        table.inner.lock().unwrap().triggers.clear();
    });
}

// The Future returned by a call to `timeout(future)`.
#[pin_project(PinnedDrop)]
pub struct Timeout<F> {
    slot: usize,
    #[pin]
    future: F,
}

impl<F> Future for Timeout<F>
where
    F: Future,
{
    type Output = Result<F::Output, error::Elapsed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // We want expiry triggers to take effect as soon as possible.  So we check
        // if a timeout has been expired before polling the underlying future.
        // If the future returns `Pending`, we poll it again in case we
        // invoked the trigger while the future was being polled.
        let slot = self.slot;
        let timed_out = TIMEOUT_TABLE.with(|table| {
            let map = table.inner.lock().unwrap();
            let entry = map.entries.get(&slot).unwrap();
            matches!(entry.state, TimeoutState::Expired)
        });
        if timed_out {
            return Poll::Ready(Err(error::Elapsed::new()));
        }
        let this = self.project();
        match this.future.poll(cx) {
            Poll::Ready(r) => Poll::Ready(Ok(r)),
            Poll::Pending => TIMEOUT_TABLE.with(|table| {
                let mut map = table.inner.lock().unwrap();
                let TimeoutEntry { state, .. } = map.entries.get_mut(&slot).unwrap();
                match state {
                    TimeoutState::Expired => Poll::Ready(Err(error::Elapsed::new())),
                    TimeoutState::Waiting(waker) => {
                        *waker = Some(cx.waker().clone());
                        Poll::Pending
                    }
                }
            }),
        }
    }
}

#[pinned_drop]
impl<F> PinnedDrop for Timeout<F> {
    fn drop(self: Pin<&mut Self>) {
        // On drop, remove the associated entry from the `TIMEOUT_TABLE`
        TIMEOUT_TABLE.with(|table| {
            let mut map = table.inner.lock().unwrap();
            if let Some(e) = map.entries.remove(&self.slot) {
                trace!("removed entry {:?} at slot {}", e, self.slot);
            }
        });
    }
}

fn timeout_inner<F>(future: F) -> Timeout<F>
where
    F: Future,
{
    let slot = {
        let task_id = get_current_task().expect("TaskId should be defined");
        // We save the debug name of the task up-front, to avoid a double-borrow of the
        // underlying Label storage in Shuttle (needed for `with_labels_for_task`).
        let task_name = format!("{task_id:?}");
        with_labels_for_task(task_id, |labels| {
            TIMEOUT_TABLE.with(|table| {
                let mut map = table.inner.lock().unwrap();
                let state = if map.triggers.iter().any(|trigger| trigger(labels)) {
                    TimeoutState::Expired
                } else {
                    TimeoutState::Waiting(None)
                };
                let slot = map.next_slot;
                map.next_slot += 1;
                trace!(
                    "Registering {}timeout for task {} in slot {}",
                    match &state {
                        TimeoutState::Expired => "(expired) ",
                        TimeoutState::Waiting(_) => "",
                    },
                    task_name,
                    slot
                );
                let entry = TimeoutEntry { task_id, state };
                map.entries.insert(slot, entry);
                slot
            })
        })
    };
    Timeout { slot, future }
}

/// Requires a `Future` to complete before the specified duration has elapsed.
pub fn timeout<T>(_duration: Duration, future: T) -> Timeout<T>
where
    T: Future,
{
    timeout_inner(future)
}

/// Requires a `Future` to complete before the specified instant in time.
pub fn timeout_at<T>(_deadline: Instant, future: T) -> Timeout<T>
where
    T: Future,
{
    timeout_inner(future)
}

// Lifted from Tokio
pub(crate) fn far_future() -> Instant {
    // Roughly 30 years from now.
    // API does not provide a way to obtain max `Instant`
    // or convert specific date in the future to instant.
    // 1000 years overflows on macOS, 100 years overflows on FreeBSD.
    Instant::now() + Duration::from_secs(86400 * 365 * 30)
}

#[pin_project]
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Sleep(#[pin] Instant);

/// Waits until duration has elapsed
pub fn sleep(duration: Duration) -> Sleep {
    match Instant::now().checked_add(duration) {
        Some(deadline) => Sleep(deadline),
        None => Sleep(far_future()),
    }
}

/// Waits until the given deadline
pub fn sleep_until(deadline: Instant) -> Sleep {
    Sleep(deadline)
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // If it is going to resolve in more than a year, return `Poll::Pending`
        if self.0 >= Instant::now() + Duration::from_secs(86400 * 365) {
            Poll::Pending
        } else {
            shuttle::thread::yield_now();
            Poll::Ready(())
        }
    }
}

impl Sleep {
    /// Returns the instant at which the future will complete.
    pub fn deadline(&self) -> Instant {
        self.0
    }

    /// Returns `true` if `Sleep` has elapsed.
    ///
    /// A `Sleep` instance is elapsed when the requested duration has elapsed.
    pub fn is_elapsed(&self) -> bool {
        Instant::now() >= self.0
    }

    /// Resets the `Sleep` instance to a new deadline.
    pub fn reset(self: Pin<&mut Self>, deadline: Instant) {
        let mut me = self.project();
        *me.0 = deadline;
    }
}

/// Advances time
pub async fn advance(_duration: Duration) {
    shuttle::future::yield_now().await;
}

/// Resumes time.
pub fn resume() {}

pub mod error {
    /// Errors returned by `Timeout`.
    ///
    /// This error is returned when a timeout expires before the function was able
    /// to finish.
    #[derive(Debug, Default, PartialEq, Eq)]
    pub struct Elapsed(());

    // ===== impl Elapsed =====

    impl Elapsed {
        // Note that this is not `pub` in Tokio. We expose it to enable modelling of timeouts.
        pub fn new() -> Self {
            Elapsed(())
        }
    }

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
}

pub use tokio::time::MissedTickBehavior;

/// Interval returned by [`interval`] and [`interval_at`].
#[derive(Debug)]
pub struct Interval {
    /// The strategy `Interval` should use when a tick is missed.
    missed_tick_behavior: MissedTickBehavior,
    count: usize,
    period: Duration,
}

/// Creates new [`Interval`] that yields with interval of `period`. The first
pub fn interval(period: Duration) -> Interval {
    interval_at(Instant::now(), period)
}

/// Creates new [`Interval`] that yields with interval of `period` with the
/// first tick completing at `start`. The default [`MissedTickBehavior`] is
/// [`Burst`](MissedTickBehavior::Burst), but this can be configured
/// by calling [`set_missed_tick_behavior`](Interval::set_missed_tick_behavior).
///
/// An interval will tick indefinitely. At any time, the [`Interval`] value can
/// be dropped. This cancels the interval.
pub fn interval_at(_start: Instant, period: Duration) -> Interval {
    // By default, Shuttle will allow interval ticks to be generated forever (or at
    // least `usize::MAX` times per interval).  To override this behavior, set the
    // environment variable `SHUTTLE_INTERVAL_TICKS` to the number of time each interval
    // should tick.  (Setting this to 0 means intervals never generate ticks.)
    let count = std::env::var("SHUTTLE_INTERVAL_TICKS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    Interval {
        missed_tick_behavior: Default::default(),
        count,
        period,
    }
}

static HAS_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Interval {
    /// Completes when the next instant in the interval has been reached.
    pub async fn tick(&mut self) -> Instant {
        if self.count > 0 {
            shuttle::future::yield_now().await;
            self.count -= 1;
            Instant::now()
        } else {
            futures::future::pending().await
        }
    }

    /// Polls for the next instant in the interval to be reached.
    pub fn poll_tick(&mut self, _cx: &mut Context<'_>) -> Poll<Instant> {
        if self.count > 0 {
            shuttle::thread::yield_now();
            self.count -= 1;
            Poll::Ready(Instant::now())
        } else {
            Poll::Pending
        }
    }

    /// Returns the [`MissedTickBehavior`] strategy currently being used.
    pub fn missed_tick_behavior(&self) -> MissedTickBehavior {
        self.missed_tick_behavior
    }

    /// Sets the [`MissedTickBehavior`] strategy that should be used.
    pub fn set_missed_tick_behavior(&mut self, behavior: MissedTickBehavior) {
        self.missed_tick_behavior = behavior;
    }

    /// Resets the interval to complete one period after the current time.
    /// TODO make this work right
    pub fn reset(&mut self) {}

    /// Disables the interval
    pub fn disable(&mut self) {
        self.count = 0;
    }

    /// Returns the period of the interval.
    pub fn period(&self) -> Duration {
        use std::sync::atomic::Ordering;
        if !HAS_WARNED.load(Ordering::SeqCst) {
            if std::env::var("SHUTTLE_SILENCE_WARNINGS").is_err() {
                tracing::warn!("`period` suggests code dependent on real time, which means that it will behave\n\
                        nondeterministically under Shuttle, meaning that failing schedules would not be replayable.\n\
                        The suggested solution is to have some different handling under Shuttle, by using `cfg(feature = \"shuttle\")`.\n
                        If you do not wish to see this warning again, then it can be turned off by setting the SHUTTLE_SILENCE_WARNINGS environment variable.");
            }
            HAS_WARNED.store(true, Ordering::SeqCst);
        }
        self.period
    }
}
