//! A model of tokio's `sync::Barrier` (tokio 1.54): the API and the semantics are tokio's, and the
//! model is `shuttle::sync::Barrier`'s, made async.
//!
//! tokio builds its barrier from a `std::sync::Mutex` and a `watch` channel. Ported as it is onto
//! this crate's `Mutex` and `watch`, every scheduling point of those would be one of `wait`'s,
//! although none of them is a step that another task can tell apart from the next: they are the
//! barrier's own bookkeeping, and they multiply the schedules to explore for nothing. So the state
//! is kept as `shuttle::sync::Barrier` keeps it, and `wait` takes one scheduling point, before the
//! arrival: the step that counts the task in, and that releases the generation if it is the `n`th.
//!
//! Every arrival takes that scheduling point, as in `shuttle::sync::Barrier`, an arrival that will
//! wait too. Arrivals that wait commute with each other, but not with the arrival that releases the
//! generation after them: which arrival comes last decides which task is the leader. Without the
//! scheduling point, the arrival would run in the same step as the task's last operation before
//! `wait`, so no other task could see that operation and still arrive first. With it, Shuttle
//! explores every order of the arrivals relative to what the tasks do before them, as threads can
//! run them.
//!
//! Where `shuttle::sync::Barrier` blocks the Shuttle task, an arrival that waits here registers a
//! waker and returns `Pending`, so that a `wait` can be cancelled, as by a `select!`, and a task can
//! wait on several barriers at once, as with `join!`.
//!
//! As in tokio (and in std and `shuttle::sync::Barrier`), the leader of a generation is the task
//! whose arrival releases it. As in `shuttle::sync::Barrier`, each arrival's vector clock goes into
//! the barrier's, which every task of the generation joins into its own when the generation is
//! released.
//!
//! As in tokio, `wait` is not cancel safe: an arrival counts toward its generation from the first
//! poll of `wait` on, so if a `wait` is dropped before its generation is released, the generation is
//! still released by the `n`th arrival, with one task fewer waiting in it.

use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::thread;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use tracing::trace;

/// A barrier enables multiple tasks to synchronize the beginning of some computation.
///
/// ```
/// # async fn dox() {
/// use shuttle_tokio_impl_inner::sync::Barrier;
/// use std::sync::Arc;
///
/// let mut handles = Vec::with_capacity(10);
/// let barrier = Arc::new(Barrier::new(10));
/// for _ in 0..10 {
///     let c = barrier.clone();
///     // The same messages will be printed together.
///     // You will NOT see any interleaving.
///     handles.push(shuttle::future::spawn(async move {
///         println!("before wait");
///         let wait_result = c.wait().await;
///         println!("after wait");
///         wait_result
///     }));
/// }
///
/// // Will not resolve until all "after wait" messages have been printed
/// let mut num_leaders = 0;
/// for handle in handles {
///     let wait_result = handle.await.unwrap();
///     if wait_result.is_leader() {
///         num_leaders += 1;
///     }
/// }
///
/// // Exactly one barrier will resolve as the "leader"
/// assert_eq!(num_leaders, 1);
/// # }
/// ```
pub struct Barrier {
    // SHUTTLE_CHANGES: tokio holds a `Mutex<BarrierState>` and the receiving end of a `watch`
    // channel here. See the module docs. The `std::sync::Mutex` only gives the state interior
    // mutability: Shuttle runs every task on one OS thread, and the lock is never held across a
    // scheduling point.
    state: Mutex<BarrierState>,
    n: usize,
}

struct BarrierState {
    /// The arrivals in the current generation so far, including those whose `wait` has been
    /// dropped since.
    arrived: usize,
    /// The number of generations released so far.
    generation: usize,
    /// The arrivals in the current generation that wait for it to be released.
    waiters: Vec<Arc<Waiter>>,
    /// The join of the clocks of every arrival so far, as in `shuttle::sync::Barrier`. Joining in
    /// the arrivals of earlier generations too is conservative, and mirrors tokio, where every
    /// arrival takes the lock that every arrival before it released.
    clock: VectorClock,
}

/// What an arrival that waits shares with the release of its generation.
#[derive(Default)]
struct Waiter(Mutex<WaiterState>);

#[derive(Default)]
struct WaiterState {
    /// The waker of the task that last polled the `wait`, while it waits.
    waker: Option<Waker>,
    /// The barrier's clock at the release of the generation, once it has been released.
    release_clock: Option<VectorClock>,
}

/// Locks one of the barrier's `std::sync::Mutex`es. None of them is held while user code runs, so
/// none can be poisoned, but the `Drop` of a `wait` must not panic while its task unwinds either.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What an arrival does once it has been counted.
enum Arrival {
    /// Waits for the generation to be released.
    Wait(Arc<Waiter>),
    /// Releases the generation: wakes these waiters, after the clocks are joined.
    Release(Vec<Waker>, VectorClock),
}

impl Barrier {
    /// Creates a new barrier that can block a given number of tasks.
    ///
    /// A barrier will block `n`-1 tasks which call [`Barrier::wait`] and then wake up all
    /// tasks at once when the `n`th task calls `wait`.
    #[track_caller]
    pub fn new(mut n: usize) -> Barrier {
        if n == 0 {
            // if n is 0, it's not clear what behavior the user wants.
            // in std::sync::Barrier, an n of 0 exhibits the same behavior as n == 1, where every
            // .wait() immediately unblocks, so we adopt that here as well.
            n = 1;
        }

        Barrier {
            state: Mutex::new(BarrierState {
                arrived: 0,
                generation: 0,
                waiters: Vec::new(),
                clock: VectorClock::new(),
            }),
            n,
        }
    }

    /// Does not resolve until all tasks have rendezvoused here.
    ///
    /// Barriers are re-usable after all tasks have rendezvoused once, and can
    /// be used continuously.
    ///
    /// A single (arbitrary) future will receive a [`BarrierWaitResult`] that returns `true` from
    /// [`BarrierWaitResult::is_leader`] when returning from this function, and all other tasks
    /// will receive a result that will return `false` from `is_leader`.
    ///
    /// # Cancel safety
    ///
    /// This method is not cancel safe.
    pub async fn wait(&self) -> BarrierWaitResult {
        // SHUTTLE_CHANGES: the scheduling point that precedes the arrival, whether it will wait or
        // release the generation (see the module docs).
        thread::switch();

        // The arrival is an event that the tasks it is released with depend on.
        let arrival_clock = ExecutionState::with(|s| s.increment_clock().clone());

        // NOTE: the extra scope here is so that the guard is dropped before the awaits below, as it
        // is in tokio, where it also keeps the returned future `Send`.
        let arrival = {
            let mut state = lock(&self.state);
            state.clock.update(&arrival_clock);
            state.arrived += 1;
            if state.arrived < self.n {
                let waiter = Arc::new(Waiter::default());
                state.waiters.push(Arc::clone(&waiter));
                trace!(
                    arrived = state.arrived,
                    n = self.n,
                    generation = state.generation,
                    "waiting on barrier {:p}",
                    self,
                );
                Arrival::Wait(waiter)
            } else {
                // we are the leader for this generation
                // wake everyone, start the next generation, and return
                trace!(
                    generation = state.generation,
                    n = self.n,
                    "releasing barrier {:p}",
                    self
                );
                state.arrived = 0;
                state.generation += 1;
                let clock = state.clock.clone();
                let wakers = std::mem::take(&mut state.waiters)
                    .iter()
                    .filter_map(|waiter| {
                        let mut waiter = lock(&waiter.0);
                        waiter.release_clock = Some(clock.clone());
                        waiter.waker.take()
                    })
                    .collect();
                Arrival::Release(wakers, clock)
            }
        };

        match arrival {
            Arrival::Wait(waiter) => {
                Wait { waiter }.await;
                BarrierWaitResult(false)
            }
            Arrival::Release(wakers, clock) => {
                // The leader's return depends on every arrival of its generation, too.
                ExecutionState::with(|s| s.update_clock(&clock));
                for waker in wakers {
                    waker.wake();
                }
                BarrierWaitResult(true)
            }
        }
    }
}

impl fmt::Debug for Barrier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = lock(&self.state);
        f.debug_struct("Barrier")
            .field("n", &self.n)
            .field("arrived", &state.arrived)
            .field("generation", &state.generation)
            .finish_non_exhaustive()
    }
}

/// The part of [`Barrier::wait`] that waits for the generation to be released.
struct Wait {
    waiter: Arc<Waiter>,
}

impl Future for Wait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // No scheduling point: the first poll comes right after the arrival, in the same step, and
        // the others after a wake-up. In either, all there is to see is whether the release has
        // happened, which the release's wake-up reports in any case.
        let mut waiter = lock(&self.waiter.0);
        match waiter.release_clock.take() {
            Some(clock) => {
                drop(waiter);
                ExecutionState::with(|s| s.update_clock(&clock));
                Poll::Ready(())
            }
            None => {
                waiter.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl Drop for Wait {
    fn drop(&mut self) {
        // SHUTTLE_CHANGES: a dropped `wait` stops listening to tokio's `watch` channel, and its
        // arrival still counts (see the module docs). Here it takes its waker back, so that the
        // release does not wake a task that no longer waits for it. That is no step that another
        // task can see, so it takes no scheduling point.
        lock(&self.waiter.0).waker = None;
    }
}

/// A `BarrierWaitResult` is returned by `wait` when all tasks in the `Barrier` have rendezvoused.
#[derive(Debug, Clone)]
pub struct BarrierWaitResult(bool);

impl BarrierWaitResult {
    /// Returns `true` if this task from wait is the "leader task".
    ///
    /// Only one task will have `true` returned from their result, all other tasks will have
    /// `false` returned.
    pub fn is_leader(&self) -> bool {
        self.0
    }
}
