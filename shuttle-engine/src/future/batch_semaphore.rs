//! A counting semaphore supporting both async and sync operations.
use crate::runtime::execution::ExecutionState;
use crate::runtime::task::{clock::VectorClock, TaskId};
use crate::runtime::thread;
use crate::sync_types::{ResourceSignature, ResourceType};
use crate::{backtrace_enabled, current};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};
use tracing::trace;

struct Waiter {
    /// The task waiting on this waiter's `Acquire`.
    ///
    /// Refreshed on every poll (like `waker`) rather than frozen at creation
    /// time. An `Acquire` future is not necessarily owned by the task that
    /// created it: it can be cached inside a longer-lived object and later
    /// polled by a different task (tokio's `poll_recv(&mut self, cx)` is the
    /// motivating example — the in-flight acquire lives in the `Receiver`, and
    /// a `Receiver` may be moved between tasks). The semaphore must unblock
    /// whoever is actually waiting now, so this follows the poller. This
    /// mirrors tokio's own `batch_semaphore`, which refreshes its waiter's
    /// `Waker` under a `will_wake` check.
    ///
    /// Stored as an atomic rather than a `Cell` to keep `Waiter` (and hence
    /// `Acquire`) `Sync`.
    task_id: AtomicUsize,
    num_permits: usize,
    /// How many permits must be available for this waiter to make progress: `num_permits` for a
    /// plain acquire, and the threshold at which it reserves the semaphore for a reserving acquire
    /// (see [`BatchSemaphore::acquire_reserving`]). Only unfair semaphores look at this.
    min_permits: usize,
    is_queued: AtomicBool,
    has_permits: AtomicBool,
    /// Clock of the task that created this waiter. Note this is *not* refreshed
    /// when `task_id` is: it is only used to seed the causality of the acquired
    /// permits, and keeping the original enqueue clock is conservative (it can
    /// only add happens-before edges, never remove them).
    clock: VectorClock,
    waker: Mutex<Option<Waker>>,
}

// Implement debug in order to not output the `VectorClock`
impl fmt::Debug for Waiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Waiter")
            .field("task_id", &self.task_id())
            .field("num_permits", &self.num_permits)
            .field("min_permits", &self.min_permits)
            .field("is_queued", &self.is_queued)
            .field("has_permits", &self.has_permits)
            .field("waker", &self.waker)
            .finish()
    }
}

impl Waiter {
    /// A `Waiter` is the part of an acquire that a *releasing* task can see and
    /// mutate, so it only needs to exist once an acquire actually blocks.
    ///
    /// `clock` is passed in rather than read from the ambient execution state,
    /// because it must be snapshotted when the `Acquire` was created, not when it
    /// later blocks: it feeds the happens-before edge recorded in
    /// `unblock_waiters_from_front`, and a scheduling point sits between those two
    /// moments. `task_id`, in contrast, tracks the current poller (see
    /// [`Waiter::task_id`]), so it is read here and refreshed on later polls.
    fn new(num_permits: usize, min_permits: usize, clock: VectorClock) -> Self {
        Self {
            task_id: AtomicUsize::new(ExecutionState::me().into()),
            num_permits,
            min_permits,
            is_queued: AtomicBool::new(false),
            has_permits: AtomicBool::new(false),
            clock,
            waker: Mutex::new(None),
        }
    }

    /// The task currently waiting on this waiter. See [`Waiter::task_id`].
    fn task_id(&self) -> TaskId {
        TaskId::from(self.task_id.load(Ordering::SeqCst))
    }

    /// Point this waiter at the task that is polling it now, so that a later
    /// `release` unblocks the current poller rather than whoever polled first.
    fn set_task_id(&self, task_id: TaskId) {
        self.task_id.store(task_id.into(), Ordering::SeqCst);
    }
}

/// Number of permits (`num_available`) available to be acquired. The permits
/// are grouped into batches in the `permit_clocks` deque, such that batches
/// farther back correspond to later `release` calls. Each batch is a tuple
/// of the permits remaining in that batch and the clock of the event whence
/// the permits originate.
struct PermitsAvailable {
    // Invariant: the number of permits available is equal to the sum of the
    // batch sizes in the queue.
    num_available: usize,

    /// Batches of permits with associated clocks (corresponding to the
    /// `release` events that created them). This is an `Option` because the
    /// deque is lazily initialized; see `const_new`.
    permit_clocks: Option<VecDeque<(usize, VectorClock)>>,

    /// The join of the clocks of the successful acquire events, and of the
    /// requests that reserved the semaphore (see
    /// [`BatchSemaphore::acquire_reserving`]). Used for causal dependence in
    /// `try_acquire` failures and in [`BatchSemaphore::load_permits`].
    last_acquire: VectorClock,

    /// The join of the clocks of the release events, and of the requests that
    /// gave up a reservation. Used for causal dependence in
    /// [`BatchSemaphore::load_permits`].
    last_release: VectorClock,
}

// Implement debug in order to not output the `VectorClock`s
impl fmt::Debug for PermitsAvailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PermitsAvailable")
            .field("num_available", &self.num_available)
            .finish()
    }
}

impl PermitsAvailable {
    fn new(num_permits: usize) -> Self {
        let mut permit_clocks = VecDeque::new();
        if num_permits > 0 {
            permit_clocks.push_back((num_permits, current::clock()));
        }
        Self {
            num_available: num_permits,
            permit_clocks: Some(permit_clocks),
            last_acquire: VectorClock::new(),
            last_release: VectorClock::new(),
        }
    }

    const fn const_new(num_permits: usize) -> Self {
        // A `VecDeque` cannot be populated in a const fn, due to allocation.
        // Instead, we set `permit_clocks` to `None`, and initialize it lazily
        // when it is needed for the first time, to contain one batch of size
        // `num_permits`.
        Self {
            num_available: num_permits,
            permit_clocks: None,
            last_acquire: VectorClock::new(),
            last_release: VectorClock::new(),
        }
    }

    fn available(&self) -> usize {
        self.num_available
    }

    fn init_permit_clocks(&mut self) {
        if self.permit_clocks.is_none() {
            let mut permit_clocks = VecDeque::new();
            if self.num_available > 0 {
                permit_clocks.push_back((self.num_available, VectorClock::new()));
            }
            self.permit_clocks = Some(permit_clocks);
        }
    }

    fn acquire(&mut self, mut num_permits: usize, acquire_clock: VectorClock) -> Result<VectorClock, TryAcquireError> {
        // Acquiring zero permits is always possible, and is not causally
        // dependent on any event.
        if num_permits == 0 {
            return Ok(VectorClock::new());
        }

        if num_permits <= self.num_available {
            self.init_permit_clocks();
            self.last_acquire.update(&acquire_clock);
            self.num_available -= num_permits;

            // Acquire `num_permits` from the available batches. This may
            // consume one or more batches from the queue. The resulting clock
            // is the join of all the batches used (fully or partially), since
            // the acquiry causally depends on the releases that created those
            // batches.
            let mut clock = VectorClock::new();
            let permit_clocks = self.permit_clocks.as_mut().unwrap();
            while let Some((batch_size, batch_clock)) = permit_clocks.front_mut() {
                clock.update(batch_clock);

                if num_permits < *batch_size {
                    // The current batch is larger than the number of permits
                    // requested: diminish batch, finish loop.
                    *batch_size -= num_permits;
                    num_permits = 0;
                } else {
                    // The current batch is fully consumed by the request.
                    // Remove it from the queue.
                    num_permits -= *batch_size;
                    permit_clocks.pop_front();
                }

                // Break early to avoid causally depending on the next batch.
                if num_permits == 0 {
                    break;
                }
            }

            assert_eq!(num_permits, 0);
            Ok(clock)
        } else {
            // There are not enough permits to fulfill the request.
            Err(TryAcquireError::NoPermits)
        }
    }

    fn release(&mut self, num_permits: usize, clock: VectorClock) {
        self.init_permit_clocks();
        self.last_release.update(&clock);
        self.num_available += num_permits;
        self.permit_clocks.as_mut().unwrap().push_back((num_permits, clock));
    }
}

/// Fairness mode for the semaphore. Determines which threads are woken when
/// permits are released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fairness {
    /// The semaphore is strictly fair, so earlier requesters always get
    /// priority over later ones.
    StrictlyFair,

    /// The semaphore makes no guarantees about fairness. In particular,
    /// a waiter can be starved by other threads.
    Unfair,
}

/// Where an acquire request sits relative to waiters that are already queued on
/// a [`Fairness::StrictlyFair`] semaphore. Ignored by an unfair semaphore, which
/// has no queue order to speak of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Priority {
    /// The default: queue behind existing waiters, and do not take available
    /// permits while any waiter is queued.
    Back,

    /// Overtake every queued waiter: take available permits even when others are
    /// waiting, and if there still aren't enough, queue at the *front*.
    ///
    /// This is only correct for a requester that already holds permits of this
    /// semaphore and is escalating its own claim (see [`BatchSemaphore::upgrade`]).
    /// Such a request cannot be satisfied by making the queue wait its turn --
    /// queued waiters hold no permits, so they can never release what the
    /// requester is missing, and the requester will not release what it holds.
    /// Deadlock is avoided precisely by letting it overtake them.
    Front,
}

/// A counting semaphore which permits waiting on multiple permits at once,
/// and supports both asychronous and synchronous blocking operations.
#[derive(Debug)]
struct BatchSemaphoreState {
    id: Option<crate::annotations::ObjectId>,

    // Key invariants:
    //
    // (1) if `waiters` is nonempty and the head waiter is `H`,
    // then `H.num_permits > permits_available.available()`.  (In other words,
    // we are never in a state where there are enough permits available for the
    // first waiter.  This invariant is ensured by the `drop` handler below.)
    //
    // (2) W is in waiters iff W.is_queued
    //
    // (3) W.is_queued ==> !W.has_permits
    // Note: the converse is not true.  We can have !W.has_permits && !W.is_queued
    // when the Acquire is created but not yet polled.
    //
    // (4) closed ==> waiters.is_empty()
    //
    // (5) if `reservation` is `Some(R)`, then the semaphore is unfair, and
    // !R.is_queued && !R.has_permits
    //
    // (6) closed ==> reservation.is_none()
    //
    // (7) W in granted ==> W.has_permits && !W.is_queued, and the semaphore is unfair
    waiters: VecDeque<Arc<Waiter>>,
    /// The waiters of an unfair semaphore that a fair release granted their permits to (see
    /// `BatchSemaphore::with_fair_releases`), and whose `Acquire` has not taken them yet. The task
    /// of such a waiter has to run to take them, so `BatchSemaphore::reblock_if_unfair` leaves it
    /// runnable.
    granted: Vec<Arc<Waiter>>,
    /// The waiter that holds the semaphore's reservation, if any (see
    /// [`BatchSemaphore::acquire_reserving`]). While it is set, the available
    /// permits are kept for this waiter: no other request can take one, and the
    /// waiter takes its `num_permits` as soon as that many are available.
    reservation: Option<Arc<Waiter>>,
    permits_available: PermitsAvailable,
    // TODO: should there be a clock for the close event?
    closed: bool,
}

impl BatchSemaphoreState {
    /// The permits that a request can take now. While a reservation holds the
    /// semaphore, that is none, except for the holder itself.
    fn available(&self) -> usize {
        if self.reservation.is_some() {
            0
        } else {
            self.permits_available.available()
        }
    }

    /// Is `waiter` the holder of the semaphore's reservation?
    fn is_reserved_by(&self, waiter: &Arc<Waiter>) -> bool {
        self.reservation.as_ref().is_some_and(|r| Arc::ptr_eq(r, waiter))
    }

    fn acquire_permits(
        &mut self,
        num_permits: usize,
        fairness: Fairness,
        priority: Priority,
    ) -> Result<(), TryAcquireError> {
        assert!(num_permits > 0);
        if self.closed {
            Err(TryAcquireError::Closed)
        } else if self.reservation.is_some() {
            // The available permits are kept for the holder of the reservation,
            // which takes them with `take_permits`.
            Err(TryAcquireError::NoPermits)
        } else if self.waiters.is_empty() || matches!(fairness, Fairness::Unfair) || priority == Priority::Front {
            // Permits here can be acquired in one of three scenarios:
            // - The waiter queue is empty; nobody else is waiting for permits,
            //   so if there are enough available, immediately succeed.
            // - The semaphore is operating in an unfair mode; the current
            //   thread is either requesting permits for the first time, or it
            //   was woken and selected by the scheduler. In either case, the
            //   thread may succeed, as long as there are enough permits.
            // - The request has `Priority::Front`, so it deliberately overtakes
            //   the queue (see `BatchSemaphore::upgrade`). Queued waiters hold
            //   no permits, so they cannot prevent this request from succeeding.
            self.take_permits(num_permits)
        } else {
            Err(TryAcquireError::NoPermits)
        }
    }

    /// Take `num_permits` of the available permits for the current task, if
    /// there are that many, regardless of the waiters and the reservation.
    fn take_permits(&mut self, num_permits: usize) -> Result<(), TryAcquireError> {
        let clock = self.permits_available.acquire(num_permits, current::clock())?;

        // If successful, the acquiry is causally dependent on the event
        // which released the acquired permits.
        ExecutionState::with(|s| {
            s.update_clock(&clock);
        });

        Ok(())
    }

    /// Unblock the waiters of an unfair semaphore that can now make progress,
    /// and let them race. While a reservation holds the semaphore, only its
    /// holder can, once enough permits are available for it.
    fn wake_unfair_waiters(&mut self) {
        if let Some(holder) = &self.reservation {
            // Like a waiter in the queue, a holder whose task has already
            // finished is stale (see `is_stale`). Drop the reservation, so
            // that it does not keep the permits from the waiters below. If the
            // `Acquire` is still alive and another task polls it, it will
            // reserve or acquire again.
            if is_stale(holder) {
                trace!("dropping stale reservation {:?} for finished task", holder);
                self.reservation = None;
            } else {
                if holder.num_permits <= self.permits_available.available() {
                    ExecutionState::with(|s| s.get_mut(holder.task_id()).unblock());
                    if let Some(waker) = holder.waker.lock().unwrap().as_ref() {
                        waker.wake_by_ref();
                    }
                }
                return;
            }
        }

        // Unblock all the waiters for which there are enough permits available,
        // then let them race.
        let num_available = self.permits_available.available();
        for waiter in &mut self.waiters {
            if waiter.min_permits <= num_available {
                // Unlike the strictly fair case, there is nothing to clean
                // up for a stale waiter (see `is_stale`): an unfair waiter
                // holds no permits, so it blocks nobody. But there is also
                // nobody to unblock.
                if !unblock_unless_stale(waiter) {
                    continue;
                }
                let maybe_waker = waiter.waker.lock().unwrap();
                if let Some(waker) = maybe_waker.as_ref() {
                    waker.wake_by_ref();
                }
            }
        }
    }

    /// Grant the waiters at the front of the queue their permits, for as long as the available
    /// permits last. On an unfair semaphore (`track_grants`), also remember each granted waiter in
    /// `granted`, until its `Acquire` takes the permits. Returns whether this granted any waiter
    /// its permits or handed one the reservation.
    fn unblock_waiters_from_front(&mut self, track_grants: bool) -> bool {
        let mut handed_over = false;
        while let Some(front) = self.waiters.front() {
            // There is nobody to unblock for a stale waiter (see `is_stale`),
            // so discard it without consuming permits; if the `Acquire` is
            // still alive and some other task polls it, it will re-acquire
            // from the (still available) permits.
            if is_stale(front) {
                let waiter = self.waiters.pop_front().unwrap();
                waiter.is_queued.store(false, Ordering::SeqCst);
                // Preserve the "queued <=> waker registered" invariant asserted
                // in `Acquire::poll`; waking a finished task's waker is a no-op.
                waiter.waker.lock().unwrap().take();
                trace!("dropping stale waiter {:?} for finished task", waiter);
                continue;
            }
            if front.num_permits <= self.permits_available.available() {
                let waiter = self.waiters.pop_front().unwrap();

                crate::annotations::record_semaphore_acquire_unblocked(
                    self.id.unwrap(),
                    waiter.task_id(),
                    waiter.num_permits,
                );

                // The clock we pass into the semaphore is the clock of the
                // waiter, corresponding to the point at which the waiter was
                // enqueued. The clock we get in return corresponds to the
                // join of the clocks of the acquired permits, used to update
                // the waiter's clock to causally depend on the release events.
                let clock = self
                    .permits_available
                    .acquire(waiter.num_permits, waiter.clock.clone())
                    .unwrap();
                trace!("granted {:?} permits to waiter {:?}", waiter.num_permits, waiter);

                // Update waiter state as it is no longer in the queue
                assert!(waiter.is_queued.swap(false, Ordering::SeqCst));
                assert!(!waiter.has_permits.swap(true, Ordering::SeqCst));
                ExecutionState::with(|s| {
                    let task = s.get_mut(waiter.task_id());
                    assert!(!task.finished());
                    // The acquiry is causally dependent on the event
                    // which released the acquired permits.
                    task.clock.update(&clock);
                    task.unblock();
                });
                let mut maybe_waker = waiter.waker.lock().unwrap();
                if let Some(waker) = maybe_waker.take() {
                    waker.wake();
                }
                drop(maybe_waker);
                if track_grants {
                    self.granted.push(waiter);
                }
                handed_over = true;
            } else if front.min_permits < front.num_permits
                && front.min_permits <= self.permits_available.available()
                && self.reservation.is_none()
            {
                // A reserving waiter that can reserve but not yet take all of its permits (only on
                // an unfair semaphore, see `BatchSemaphore::acquire_reserving`). Hand it the
                // reservation, so that no later request can overtake it either, and stop: the
                // reservation keeps the rest of the permits for it. Its task stays blocked until
                // the permits are there, as for a reservation it takes itself.
                let waiter = self.waiters.pop_front().unwrap();
                assert!(waiter.is_queued.swap(false, Ordering::SeqCst));
                trace!("handed the reservation to waiter {:?}", waiter);
                // A request that the reservation refuses is after the request that holds it, and
                // after the release that handed it over (see `try_acquire`).
                self.permits_available.last_acquire.update(&waiter.clock);
                self.permits_available.last_acquire.update(&current::clock());
                self.reservation = Some(waiter);
                return true;
            } else {
                break;
            }
        }
        handed_over
    }
}

/// Whether `waiter` is stale: the task that registered it has finished, after its `Acquire` future
/// was cancelled (e.g. a `select!` branch lost, or a `poll_recv`-style API cached the `Acquire`
/// inside a longer-lived object). If the `Acquire` is still alive, another task can poll it again.
/// Can't tell outside an execution, and then says no, which preserves the old behaviour.
#[inline]
fn is_stale(waiter: &Waiter) -> bool {
    ExecutionState::try_with(|s| s.try_get(waiter.task_id()).is_some_and(|task| task.finished())).unwrap_or(false)
}

/// Unblock the task that registered `waiter`, unless the waiter is stale (see `is_stale`). Returns
/// whether it unblocked the task.
#[inline]
fn unblock_unless_stale(waiter: &Waiter) -> bool {
    ExecutionState::with(|s| {
        let task = s.get_mut(waiter.task_id());
        if task.finished() {
            false
        } else {
            task.unblock();
            true
        }
    })
}

/// Counting semaphore
#[derive(Debug)]
pub struct BatchSemaphore {
    state: RefCell<BatchSemaphoreState>,
    fairness: Fairness,
    /// The fairness of [`BatchSemaphore::release`]: `fairness`, unless an unfair semaphore is built
    /// [`BatchSemaphore::with_fair_releases`].
    release_fairness: Fairness,
    #[allow(unused)]
    signature: ResourceSignature,
}

/// Error returned from the [`BatchSemaphore::try_acquire`] function.
#[derive(Debug, PartialEq, Eq)]
pub enum TryAcquireError {
    /// The semaphore has been closed and cannot issue new permits.
    Closed,

    /// The semaphore has no available permits.
    NoPermits,
}

/// Error returned from the [`BatchSemaphore::acquire`] function.
///
/// An `acquire*` operation can only fail if the semaphore has been
/// closed.
#[derive(Debug)]
pub struct AcquireError(());

impl AcquireError {
    fn closed() -> AcquireError {
        AcquireError(())
    }
}

impl fmt::Display for AcquireError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fmt, "semaphore closed")
    }
}

impl std::error::Error for AcquireError {}

impl BatchSemaphore {
    /// Creates a new semaphore with the initial number of permits.
    #[track_caller]
    pub fn new(num_permits: usize, fairness: Fairness) -> Self {
        Self::new_with_signature(
            num_permits,
            fairness,
            ExecutionState::new_resource_signature(ResourceType::BatchSemaphore),
        )
    }

    pub fn new_with_signature(num_permits: usize, fairness: Fairness, signature: ResourceSignature) -> Self {
        let state = RefCell::new(BatchSemaphoreState {
            id: Some(crate::annotations::record_semaphore_created()),
            waiters: VecDeque::new(),
            granted: Vec::new(),
            reservation: None,
            permits_available: PermitsAvailable::new(num_permits),
            closed: false,
        });
        Self {
            state,
            fairness,
            release_fairness: fairness,
            signature,
        }
    }

    /// Creates a new semaphore with the initial number of permits.
    #[track_caller]
    pub const fn const_new(num_permits: usize, fairness: Fairness) -> Self {
        Self::const_new_with_signature(
            num_permits,
            fairness,
            ResourceSignature::new_const(ResourceType::BatchSemaphore),
        )
    }

    pub const fn const_new_with_signature(
        num_permits: usize,
        fairness: Fairness,
        signature: ResourceSignature,
    ) -> Self {
        let state = RefCell::new(BatchSemaphoreState {
            id: None,
            waiters: VecDeque::new(),
            granted: Vec::new(),
            reservation: None,
            permits_available: PermitsAvailable::const_new(num_permits),
            closed: false,
        });
        Self {
            state,
            fairness,
            release_fairness: fairness,
            signature,
        }
    }

    /// Makes every [`BatchSemaphore::release`] of this semaphore a fair release. Requests are still
    /// matched against the free permits as on an unfair semaphore, and reservations still work (see
    /// [`BatchSemaphore::acquire_reserving`]), but a release grants the permits to the
    /// longest-waiting requests, so that no other request can overtake them.
    ///
    /// A plain release of an unfair semaphore wakes the waiters that the permits could satisfy and
    /// lets every request race for them, so a request that was not even waiting can take the
    /// permits first. A fair release instead grants waiting requests their permits inside the
    /// release itself, from the front of the queue (the longest-waiting request first), and stops
    /// at the first waiter whose request does not fit the permits that are left. No scheduling
    /// point separates the release from those grants, so nothing can overtake them. A reserving
    /// waiter at the front whose request does not fit yet, but which can reserve the semaphore, is
    /// handed the reservation, which also stops the grants. Waiters that the remaining permits could
    /// satisfy are then woken to race for them as usual. While a reservation holds the semaphore,
    /// the permits are already kept for its holder, so a release grants nothing.
    ///
    /// The order of the waiters then decides who is granted permits, so a blocking acquire has a
    /// scheduling point before it joins the queue, as on a strictly fair semaphore. Shuttle then
    /// explores every order in which tasks can join it.
    ///
    /// The motivating use case is a `parking_lot` `RwLock` that is as fair as Shuttle's other
    /// locks: a request that `parking_lot` lets in at once never waits behind another one, but a
    /// task that waits gets the lock before any later request. `parking_lot`'s fair unlock hands
    /// the lock to the parked threads in much the same way, and hands a writer `WRITER_BIT` while it
    /// waits for the readers to leave, but it does not always stop at the first thread that does not
    /// fit: after an upgradable reader, it skips the writers and upgradable readers behind it and
    /// hands the lock to the plain readers behind those too. Here those readers are only woken, to
    /// race for the permits. The outcomes are the same, since `parking_lot` gives the ones of a
    /// reader that parks only after the unlock too.
    ///
    /// On a strictly fair semaphore, every release is already fair, so this changes nothing.
    ///
    /// Hidden and deprecated: this exists only so that `shuttle-parking_lot`'s `RwLock` keeps
    /// modelling every unlock as fair until it models `parking_lot`'s unfair unlocks (#259). Do not
    /// use it anywhere else.
    #[doc(hidden)]
    #[deprecated(note = "only for `shuttle-parking_lot`'s `RwLock`; do not use it")]
    pub const fn with_fair_releases(mut self) -> Self {
        self.release_fairness = Fairness::StrictlyFair;
        self
    }

    /// Returns the current number of available permits. While a reservation
    /// holds the semaphore (see [`BatchSemaphore::acquire_reserving`]), this is
    /// zero: the available permits are kept for the holder.
    pub fn available_permits(&self) -> usize {
        let state = self.state.borrow();
        state.available()
    }

    /// Reads the semaphore's state in one scheduling point: `None` if the
    /// semaphore is closed, otherwise `Some` of the permits a request could
    /// take now (zero while a reservation lasts, as for
    /// [`BatchSemaphore::available_permits`]).
    ///
    /// This models a load of the atomic word that a real lock keeps, like
    /// `parking_lot`'s `is_locked`: other tasks can run before the read (at the
    /// scheduling point), but the read itself changes nothing. Probing with
    /// `try_acquire` and releasing instead would transiently hold the permits
    /// across one or two more scheduling points, where another task could
    /// observe a state that the real lock never shows.
    ///
    /// Both parts of the result describe the same instant: no scheduling point
    /// separates the closed check from the permit count.
    ///
    /// The read is causally after every acquire and release of the semaphore
    /// before it, and after the requests that reserved the semaphore or gave a
    /// reservation up, as a load of an atomic word is after the stores to it.
    /// So a schedule replayed up to the read's clock (see
    /// `ReplayScheduler::set_target_clock`) reads the same state.
    pub fn load_permits(&self) -> Option<usize> {
        thread::switch();

        let state = self.state.borrow();
        ExecutionState::with(|s| {
            s.update_clock(&state.permits_available.last_acquire);
            s.update_clock(&state.permits_available.last_release);
        });
        if state.closed {
            None
        } else {
            Some(state.available())
        }
    }

    fn init_object_id(&self) {
        let mut state = self.state.borrow_mut();
        if state.id.is_none() {
            state.id = Some(crate::annotations::record_semaphore_created());
        }
    }

    /// Closes the semaphore. This prevents the semaphore from issuing new
    /// permits and notifies all pending waiters.
    pub fn close(&self) {
        thread::switch();
        self.close_no_scheduling_point();
    }

    /// Closes the semaphore without invoking `thread::switch`
    pub fn close_no_scheduling_point(&self) {
        self.init_object_id();
        let mut state = self.state.borrow_mut();
        if state.closed {
            return;
        }
        crate::annotations::record_semaphore_closed(state.id.unwrap());
        state.closed = true;

        // Wake up all the waiters, and the holder of the reservation, which waits
        // too.  Since we've marked the state as closed, they will all return
        // `AcquireError::closed` from their acquire calls.
        let ptr = &*state as *const BatchSemaphoreState;
        let holder = state.reservation.take();
        let queued = state
            .waiters
            .drain(..)
            .inspect(|waiter| assert!(waiter.is_queued.swap(false, Ordering::SeqCst)));
        for waiter in queued.chain(holder) {
            trace!(
                "semaphore {:p} removing and waking up waiter {:?} on close",
                ptr,
                waiter,
            );
            assert!(!waiter.has_permits.load(Ordering::SeqCst)); // sanity check
                                                                 // There is nothing to unblock for a stale waiter (see `is_stale`).
            unblock_unless_stale(&waiter);
            let mut maybe_waker = waiter.waker.lock().unwrap();
            if let Some(waker) = maybe_waker.take() {
                waker.wake();
            }
        }
    }

    /// Returns true iff the semaphore is closed.
    pub fn is_closed(&self) -> bool {
        let state = self.state.borrow();
        state.closed
    }

    /// Try to acquire the specified number of permits from the Semaphore.
    /// If the permits are available, returns Ok(())
    /// If the semaphore is closed, returns `Err(TryAcquireError::Closed)`
    /// If there aren't enough permits, returns `Err(TryAcquireError::NoPermits)`
    pub fn try_acquire(&self, num_permits: usize) -> Result<(), TryAcquireError> {
        thread::switch();

        self.init_object_id();
        let mut state = self.state.borrow_mut();
        let id = state.id.unwrap();
        let res = state
            .acquire_permits(num_permits, self.fairness, Priority::Back)
            .inspect_err(|_err| {
                // Conservatively, the requester causally depends on the
                // last successful acquire, and on the request that holds the
                // reservation, if one refused it.
                // TODO: This is not precise, but `try_acquire` causal dependency
                // TODO: is both hard to define, and is most likely not worth the
                // TODO: effort. The cases where causality would be tracked
                // TODO: "imprecisely" do not correspond to commonly used sync.
                // TODO: primitives, such as mutexes, mutexes, or condvars.
                // TODO: An example would be a counting semaphore used to guard
                // TODO: access to N homogenous resources (as opposed to FIFO,
                // TODO: heterogenous resources).
                // TODO: More precision could be gained by tracking clocks for all
                // TODO: current permit holders, with a data structure similar to
                // TODO: `permits_available`.
                ExecutionState::with(|s| {
                    s.update_clock(&state.permits_available.last_acquire);
                });
            });
        drop(state);

        // If we won the race for permits of an unfair semaphore, re-block
        // other waiting threads that can no longer succeed.
        if res.is_ok() {
            self.reblock_if_unfair();
        }

        crate::annotations::record_semaphore_try_acquire(id, num_permits, res.is_ok());

        res
    }

    /// Clean-up method used when a thread succeeds in acquiring permits, or
    /// when a release grants them. If the semaphore is unfair, a preceding
    /// `release` may have woken a number of tasks, some of which may no longer
    /// be able to succeed with the permits remaining in the semaphore. Those
    /// go back to sleep, as they were before the release woke them.
    fn reblock_if_unfair(&self) {
        if self.fairness == Fairness::Unfair {
            let state = self.state.borrow_mut();
            if state.waiters.is_empty() {
                return;
            }
            // A queued waiter cannot make progress while a reservation keeps
            // the available permits.
            let fits = |waiter: &Waiter| {
                state.reservation.is_none() && waiter.min_permits <= state.permits_available.available()
            };
            ExecutionState::with(|s| {
                let me = s.try_current().map(|task| task.id());
                for waiter in &state.waiters {
                    let task = waiter.task_id();
                    // Only a task that a release woke, and that has not run
                    // since, is runnable here. A task that waits for its
                    // `Acquire` sleeps (see `block_on`), and one that is blocked
                    // waits for something else: there is nothing to undo, and
                    // blocking either would lose the wakes of its other futures.
                    // That also skips stale waiters (see `is_stale`). And skip the
                    // current task's own waiters: it is running, which an
                    // `Acquire` of its that is still queued doesn't change.
                    if fits(waiter) || Some(task) == me || !s.try_get(task).is_some_and(|t| t.runnable()) {
                        continue;
                    }
                    // The task can still make progress if another `Acquire` of
                    // its can: one that a fair release granted permits to, which
                    // it has to run to take, or a queued one that fits.
                    let can_progress = state.granted.iter().any(|w| w.task_id() == task)
                        || state.waiters.iter().any(|w| w.task_id() == task && fits(w));
                    if !can_progress {
                        // Put the task back to sleep: this waiter cannot succeed
                        // (there are not enough permits available), and its
                        // `poll` would return without resolving. A wake of any
                        // of the task's futures wakes it again.
                        s.get_mut(task).sleep();
                    }
                }
            });
        }
    }

    fn enqueue_waiter(&self, waiter: &Arc<Waiter>, priority: Priority) {
        let mut state = self.state.borrow_mut();

        trace!(
            "enqueuing waiter {:?} ({priority:?}) for semaphore {:p}",
            waiter,
            &self.state
        );
        match priority {
            Priority::Back => state.waiters.push_back(waiter.clone()),
            // Overtakes the queue rather than joining its tail. Key invariant (1)
            // still holds: we only get here because the acquire failed, and a
            // `Priority::Front` acquire only fails when there really aren't
            // enough permits available, so the new head cannot be grantable.
            Priority::Front => state.waiters.push_front(waiter.clone()),
        }

        assert!(!waiter.has_permits.load(Ordering::SeqCst));
        assert!(!waiter.is_queued.swap(true, Ordering::SeqCst));
    }

    fn remove_waiter(&self, waiter: &Arc<Waiter>) {
        let mut state = self.state.borrow_mut();

        trace!(waiters = ?state.waiters, "removing waiter {:?} from semaphore {:p}", waiter, &self.state);

        // sanity checks
        assert!(!state.closed);
        assert!(!waiter.has_permits.load(Ordering::SeqCst));

        let index = state
            .waiters
            .iter()
            .position(|x| Arc::ptr_eq(x, waiter))
            .expect("did not find waiter");

        state.waiters.remove(index).unwrap();
        assert!(waiter.is_queued.swap(false, Ordering::SeqCst));

        match self.fairness {
            Fairness::StrictlyFair => {
                if index == 0 {
                    // If the semaphore is strictly fair, and we removed the first waiter, check if its
                    // removal unblocks remaining waiters.  This can happen in the following situation:
                    // - the semahore has 1 permit available
                    // - there are 2 waiters W1 and W2 where W1 wants 2 permits, and W2 wants 1 permit
                    // - if W1 gives up and drops out, we want to ensure W2 is granted the semaphore
                    state.unblock_waiters_from_front(false);
                }
            }
            Fairness::Unfair => {}
        }
    }

    /// End the reservation that `waiter` holds, because its `Acquire` was
    /// dropped before it was granted. The permits that the reservation kept
    /// were never taken, so they are available again at once.
    fn cancel_reservation(&self, waiter: &Arc<Waiter>) {
        let mut state = self.state.borrow_mut();

        trace!("cancelling reservation {:?} of semaphore {:p}", waiter, &self.state);

        assert!(state.is_reserved_by(waiter));
        state.reservation = None;
        // Giving up the reservation changes the state that `load_permits` reads.
        state.permits_available.last_release.update(&current::clock());

        // Wake the waiters that can now take the permits, unless `release`
        // wouldn't either (see `ExecutionState::should_stop`).
        let can_wake = ExecutionState::try_with(|s| !s.stops(std::thread::panicking())).unwrap_or(false);
        if can_wake {
            state.wake_unfair_waiters();
        }
    }

    /// Acquire the specified number of permits (async API)
    pub fn acquire(&self, num_permits: usize) -> Acquire<'_> {
        // No switch here; switch should be triggered on polling future
        self.init_object_id();
        Acquire::new(self, num_permits, Priority::Back)
    }

    /// Acquire the specified number of permits (blocking API)
    pub fn acquire_blocking(&self, num_permits: usize) -> Result<(), AcquireError> {
        crate::future::block_on(self.acquire(num_permits))
    }

    /// Acquire `num_permits` permits, and reserve the semaphore for this request
    /// as soon as at least `min_permits` permits are available (async API). Only
    /// an unfair semaphore supports this.
    ///
    /// Until `min_permits` permits are available, the request waits like one
    /// from [`BatchSemaphore::acquire`]: it holds nothing and stops no other
    /// request. As soon as they are, it reserves the semaphore in the same step.
    /// From then on, no other request can take a permit, and this request takes
    /// its `num_permits` as soon as that many are available. The reservation
    /// ends when the request is granted, or when the returned future is dropped.
    /// While it lasts, [`BatchSemaphore::available_permits`] is zero.
    ///
    /// The motivating use case is a `parking_lot` `RwLock` writer. `parking_lot`
    /// sets `WRITER_BIT` only when no writer or upgradable reader holds the lock,
    /// and from then on, the bit stops new readers while the writer waits for
    /// the current ones to leave. With `min_permits` above the permits that are
    /// left while an upgradable reader holds the lock, the reservation is that
    /// bit.
    ///
    /// At most one request can hold the reservation. Another reserving request
    /// waits like any other until the reservation ends.
    ///
    /// # Panics
    ///
    /// Panics if the semaphore is strictly fair (its queue already keeps the
    /// permits for its first waiter), if `num_permits` is zero, or if
    /// `min_permits > num_permits`.
    pub fn acquire_reserving(&self, min_permits: usize, num_permits: usize) -> Acquire<'_> {
        assert_eq!(
            self.fairness,
            Fairness::Unfair,
            "only an unfair semaphore supports reservations"
        );
        assert!(num_permits > 0);
        assert!(min_permits <= num_permits);

        self.init_object_id();
        Acquire::new_reserving(self, num_permits, min_permits)
    }

    /// Release `num_permits` back to the Semaphore. On a strictly fair semaphore, or one built
    /// [`BatchSemaphore::with_fair_releases`], this grants the permits to the waiters at the front
    /// of the queue; otherwise it wakes the waiters that they could satisfy, to race for them.
    pub fn release(&self, num_permits: usize) {
        // Execution teardown can unwind a task's stack from this scheduling point, which is often in
        // a destructor that releases a lock (see `ExecutionState::tear_down`). The permits must not
        // be lost then, as destructors that run later can need them.
        struct ReleaseOnUnwind<'a>(&'a BatchSemaphore, usize);
        impl Drop for ReleaseOnUnwind<'_> {
            fn drop(&mut self) {
                self.0.release_no_scheduling_point(self.1);
            }
        }
        let release_on_unwind = ReleaseOnUnwind(self, num_permits);
        thread::switch();
        std::mem::forget(release_on_unwind);

        self.release_no_scheduling_point(num_permits);
    }

    /// `release` without its scheduling point.
    #[inline]
    fn release_no_scheduling_point(&self, num_permits: usize) {
        self.init_object_id();
        if num_permits == 0 {
            return;
        }

        let mut state = self.state.borrow_mut();

        crate::annotations::record_semaphore_release(state.id.unwrap(), num_permits);

        if ExecutionState::should_stop() {
            // In case we are panicking, we release permits, but also clear
            // the waiters queue: we should not unblock the threads at this
            // point. However, the permits are released such that future
            // acquires may succeed, as long as the requesters were not
            // blocking on the semaphore at the time of the panic. This is
            // used to correctly model lock poisoning.
            state.permits_available.release(num_permits, VectorClock::new());
            for waiter in &state.waiters {
                waiter.is_queued.swap(false, Ordering::SeqCst);
            }
            state.waiters.clear();
            state.reservation = None;
            state.closed = true;
            return;
        }

        // Permits released into the semaphore reflect the releasing thread's
        // clock; future acquires of those permits are causally dependent on
        // this event.
        ExecutionState::with(|s| {
            let clock = s.increment_clock();
            state.permits_available.release(num_permits, clock.clone());
        });

        // `ExecutionState::me()` is only wanted for this trace, so let the macro's
        // level check decide whether to pay for it. Computing it eagerly cost an
        // `ExecutionState::with` on every release even with tracing disabled.
        trace!(task = ?ExecutionState::me(), avail = ?state.permits_available, waiters = ?state.waiters, "released {} permits for semaphore {:p}", num_permits, &self.state);

        let handed_over = match self.fairness {
            Fairness::StrictlyFair => {
                // in a strictly fair mode we will grant permits to waiters from the front
                // of the queue, as long as there are enough permits available
                state.unblock_waiters_from_front(false);
                false
            }
            Fairness::Unfair => {
                // A fair release grants waiting requests their permits here, inside the
                // release, so that no other request can overtake them (see
                // `with_fair_releases`). Not while a reservation holds the semaphore: the
                // permits are already kept for its holder, which `wake_unfair_waiters` takes
                // care of below.
                let handed_over = self.release_fairness == Fairness::StrictlyFair
                    && state.reservation.is_none()
                    && state.unblock_waiters_from_front(true);
                // in an unfair mode, we will unblock all the waiters for which
                // there are enough permits available, then let them race
                state.wake_unfair_waiters();
                handed_over
            }
        };
        drop(state);

        // The grants, or the reservation that they handed over, can leave a task that an earlier
        // release woke unable to make progress.
        if handed_over {
            self.reblock_if_unfair();
        }
    }

    /// Atomically `upgrade` from holding `permits_currently_held` permits to holding
    /// `permits_to_be_held`, without ever dropping below `permits_currently_held` in between.
    /// The motivating use case is `parking_lot`'s `RwLockUpgradableReadGuard::upgrade`, which must
    /// take a read guard to a write guard without letting any writer in along the way.
    ///
    /// This is implemented by acquiring only the *missing* permits
    /// (`permits_to_be_held - permits_currently_held`), with priority over any waiter already
    /// queued, so that the request overtakes the queue. Both halves of that matter:
    ///
    /// * Keeping the held permits means no other task can claim the resource mid-upgrade. Releasing
    ///   them first (even for an instant) would hand the resource to a queued waiter, which for an
    ///   `RwLock` means a writer mutating the data an upgradable reader had already observed.
    /// * Overtaking the queue is what makes that safe rather than deadlock-prone. Since we hold
    ///   permits we will not release, a queued waiter ahead of us may be unsatisfiable (an `RwLock`
    ///   writer wants *all* permits), so waiting our turn behind it could deadlock. Queued waiters
    ///   hold no permits, so overtaking them costs nothing but their place in line -- which is
    ///   exactly the priority a real upgradable read lock gives an upgrade.
    ///
    /// The upgrade therefore blocks only on tasks that *currently hold* permits, and is granted as
    /// soon as they release. The returned future must be driven to completion; if it is dropped
    /// first, the caller still holds `permits_currently_held`.
    ///
    /// An unfair semaphore has no queue to overtake, so there the upgrade reserves the semaphore
    /// instead (see [`BatchSemaphore::acquire_reserving`]), at once unless another request holds
    /// the reservation. From then on, no other request can take a permit, so the upgrade again
    /// waits only for the tasks that hold permits, and nothing can overtake it.
    ///
    /// At most one `upgrade` may be in flight on a semaphore at a time. Two concurrent upgraders
    /// could each be waiting for permits the other holds, which no queue discipline can resolve.
    /// Callers are expected to enforce this (an `RwLock` does: there is only ever one upgradable
    /// reader).
    pub fn upgrade(&self, permits_currently_held: usize, permits_to_be_held: usize) -> Acquire<'_> {
        assert!(permits_currently_held > 0);
        assert!(permits_to_be_held > permits_currently_held);

        self.init_object_id();
        let num_permits = permits_to_be_held - permits_currently_held;
        match self.fairness {
            Fairness::StrictlyFair => Acquire::new(self, num_permits, Priority::Front),
            Fairness::Unfair => Acquire::new_reserving(self, num_permits, 0),
        }
    }

    /// The non-blocking analogue of [`BatchSemaphore::upgrade`]: succeeds only if the missing
    /// permits are available right now, and never blocks or queues.
    ///
    /// Like `upgrade`, this ignores queued waiters (they hold no permits, so they cannot be the
    /// reason the upgrade is short of permits). A `try_upgrade` therefore fails only when some
    /// other task actually *holds* permits the upgrade needs.
    pub fn try_upgrade(&self, permits_currently_held: usize, permits_to_be_held: usize) -> Result<(), TryAcquireError> {
        assert!(permits_currently_held > 0);
        assert!(permits_to_be_held > permits_currently_held);

        thread::switch();

        self.init_object_id();
        let num_permits = permits_to_be_held - permits_currently_held;
        let mut state = self.state.borrow_mut();
        let id = state.id.unwrap();
        let res = state
            .acquire_permits(num_permits, self.fairness, Priority::Front)
            .inspect_err(|_err| {
                // Conservatively, the requester causally depends on the last successful acquire;
                // see the equivalent reasoning in `try_acquire`.
                ExecutionState::with(|s| {
                    s.update_clock(&state.permits_available.last_acquire);
                });
            });
        drop(state);

        // If we took permits from an unfair semaphore, re-block waiting threads that can no longer
        // succeed.
        if res.is_ok() {
            self.reblock_if_unfair();
        }

        crate::annotations::record_semaphore_try_acquire(id, num_permits, res.is_ok());

        res
    }
}

// SAFETY: Semaphore is never actually passed across true threads, only across continuations. The
// RefCell<_> type therefore can't be preempted mid-bookkeeping-operation.
// TODO we shouldn't need to do this, but RefCell is not Send, and anything we put within a Semaphore
// TODO needs to be Send.
unsafe impl Send for BatchSemaphore {}
unsafe impl Sync for BatchSemaphore {}

impl Default for BatchSemaphore {
    #[track_caller]
    fn default() -> Self {
        Self::new(Default::default(), Fairness::StrictlyFair)
    }
}

/// The future that results from async calls to `acquire*`.
/// Callers must `await` on this future to obtain the necessary permits.
pub struct Acquire<'a> {
    semaphore: &'a BatchSemaphore,
    num_permits: usize,

    /// Where this acquire sits relative to waiters already queued on a fair
    /// semaphore. Only [`BatchSemaphore::upgrade`] uses [`Priority::Front`]; see
    /// there for why an upgrade must overtake the queue.
    priority: Priority,

    /// For a reserving acquire, the number of available permits at which it
    /// reserves the semaphore (see [`BatchSemaphore::acquire_reserving`]).
    /// `None` for every other acquire.
    reserve_at: Option<usize>,

    /// Snapshotted when this `Acquire` is created, and moved into the `Waiter` if
    /// this acquire ends up blocking. See `Waiter::new` for why the snapshot must
    /// happen here rather than at enqueue time.
    clock: VectorClock,

    /// The shared part of this acquire, allocated only once the acquire has to
    /// block. An acquire that gets its permits immediately is never visible to
    /// any other task, so it needs no shared state and no allocation. While this
    /// is `None`, `has_permits` below is authoritative.
    waiter: Option<Arc<Waiter>>,

    /// Whether permits have been granted, for the case where no `Waiter` exists.
    /// Once one does, the releasing task writes `Waiter::has_permits` instead and
    /// this field is unused; read through `Acquire::has_permits`.
    has_permits: bool,

    completed: bool, // Has the future completed yet?
    never_polled: bool,
}

// Implement Debug in order to not output the `VectorClock`, matching `Waiter`.
impl fmt::Debug for Acquire<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Acquire")
            .field("num_permits", &self.num_permits)
            .field("priority", &self.priority)
            .field("reserve_at", &self.reserve_at)
            .field("waiter", &self.waiter)
            .field("has_permits", &self.has_permits())
            .field("completed", &self.completed)
            .finish()
    }
}

impl<'a> Acquire<'a> {
    fn new(semaphore: &'a BatchSemaphore, num_permits: usize, priority: Priority) -> Self {
        Self {
            semaphore,
            num_permits,
            priority,
            reserve_at: None,
            clock: current::clock(),
            waiter: None,
            has_permits: false,
            completed: false,
            never_polled: true,
        }
    }

    fn new_reserving(semaphore: &'a BatchSemaphore, num_permits: usize, min_permits: usize) -> Self {
        let mut acquire = Self::new(semaphore, num_permits, Priority::Back);
        acquire.reserve_at = Some(min_permits);
        acquire
    }

    /// How many permits must be available for this acquire to make progress
    /// (see `Waiter::min_permits`).
    fn min_permits(&self) -> usize {
        self.reserve_at.unwrap_or(self.num_permits)
    }

    /// Does this acquire hold the semaphore's reservation?
    fn is_reserving(&self) -> bool {
        self.waiter
            .as_ref()
            .is_some_and(|waiter| self.semaphore.state.borrow().is_reserved_by(waiter))
    }

    /// Have permits been granted to this acquire? Once a `Waiter` exists the
    /// releasing task owns that flag, so the shared copy is authoritative.
    fn has_permits(&self) -> bool {
        match &self.waiter {
            Some(waiter) => waiter.has_permits.load(Ordering::SeqCst),
            None => self.has_permits,
        }
    }

    /// Is this acquire in the semaphore's waiter queue? Only possible once a
    /// `Waiter` has been allocated, since the queue holds `Arc<Waiter>`.
    fn is_queued(&self) -> bool {
        match &self.waiter {
            Some(waiter) => waiter.is_queued.load(Ordering::SeqCst),
            None => false,
        }
    }

    /// Forget that a fair release granted this acquire its permits (see
    /// `BatchSemaphoreState::granted`), now that the acquire takes them or gives
    /// them back.
    fn forget_grant(&self) {
        if let Some(waiter) = &self.waiter {
            let mut state = self.semaphore.state.borrow_mut();
            if let Some(index) = state.granted.iter().position(|w| Arc::ptr_eq(w, waiter)) {
                state.granted.swap_remove(index);
            }
        }
    }

    fn grant_permits(&mut self) {
        match &self.waiter {
            Some(waiter) => waiter.has_permits.store(true, Ordering::SeqCst),
            None => self.has_permits = true,
        }
    }

    /// The shared `Waiter` for this acquire, allocating it if this is the first
    /// time the acquire has had to block. Returns an owned handle so callers can
    /// still use `self.semaphore` without holding a borrow of `self`.
    fn waiter_for_blocking(&mut self) -> Arc<Waiter> {
        if let Some(waiter) = &self.waiter {
            return Arc::clone(waiter);
        }
        let waiter = Arc::new(Waiter::new(self.num_permits, self.min_permits(), self.clock.clone()));
        self.waiter = Some(Arc::clone(&waiter));
        waiter
    }

    /// The part of `poll` for a reserving acquire (see
    /// [`BatchSemaphore::acquire_reserving`]), once `poll` knows that it has no
    /// permits yet and that the semaphore is open. Only an unfair semaphore has
    /// reserving acquires.
    fn poll_reserving(&mut self, min_permits: usize, cx: &mut Context<'_>) -> Poll<Result<(), AcquireError>> {
        let semaphore = self.semaphore;
        let is_queued = self.is_queued();
        let is_reserving = self.is_reserving();
        trace!(
            "Acquire::poll for reserving {:?}; is queued: {is_queued:?}, is reserving: {is_reserving:?}",
            self
        );

        let mut state = semaphore.state.borrow_mut();
        let id = state.id.unwrap();
        let available = state.permits_available.available();

        if is_reserving {
            if available < self.num_permits {
                // Still waiting for the tasks that hold the rest. Like a queued
                // waiter, follow the current poller.
                drop(state);
                let waiter = self.waiter_for_blocking();
                *waiter.waker.lock().unwrap() = Some(cx.waker().clone());
                waiter.set_task_id(ExecutionState::me());
                return Poll::Pending;
            }

            // The reservation kept the permits for us, so take them.
            state.reservation = None;
            state.take_permits(self.num_permits).unwrap();
            // Let the waiters race for any permits that are left.
            state.wake_unfair_waiters();
            drop(state);

            let waiter = self
                .waiter
                .clone()
                .expect("a reserving acquire must have an allocated waiter");
            crate::annotations::record_semaphore_acquire_unblocked(id, waiter.task_id(), self.num_permits);
            self.grant_permits();
            self.completed = true;
            trace!("Acquire::poll for {:?} that got permits", self);
            return Poll::Ready(Ok(()));
        }

        if state.reservation.is_some() || available < min_permits {
            // Wait, holding nothing, like any other waiter of an unfair
            // semaphore.
            drop(state);
            let waiter = self.waiter_for_blocking();
            *waiter.waker.lock().unwrap() = Some(cx.waker().clone());
            waiter.set_task_id(ExecutionState::me());
            if !is_queued {
                crate::annotations::record_semaphore_acquire_blocked(id, self.num_permits);
                semaphore.enqueue_waiter(&waiter, Priority::Back);
            }
            trace!("Acquire::poll for {:?} that is enqueued", self);
            return Poll::Pending;
        }

        if available >= self.num_permits {
            // There are enough permits, so there is nothing to reserve.
            state.take_permits(self.num_permits).unwrap();
            drop(state);
            if is_queued {
                let waiter = self
                    .waiter
                    .clone()
                    .expect("a queued acquire must have an allocated waiter");
                crate::annotations::record_semaphore_acquire_unblocked(id, waiter.task_id(), self.num_permits);
                semaphore.remove_waiter(&waiter);
            } else {
                crate::annotations::record_semaphore_acquire_fast(id, self.num_permits);
            }
            self.grant_permits();
            self.completed = true;
            trace!("Acquire::poll for {:?} that got permits", self);
            semaphore.reblock_if_unfair();
            return Poll::Ready(Ok(()));
        }

        // Reserve the semaphore, and wait for the rest of the permits.
        drop(state);
        let waiter = self.waiter_for_blocking();
        *waiter.waker.lock().unwrap() = Some(cx.waker().clone());
        waiter.set_task_id(ExecutionState::me());
        if is_queued {
            semaphore.remove_waiter(&waiter);
        } else {
            crate::annotations::record_semaphore_acquire_blocked(id, self.num_permits);
        }
        let mut state = semaphore.state.borrow_mut();
        // A request that the reservation refuses is after this one (see `try_acquire`).
        state.permits_available.last_acquire.update(&current::clock());
        state.reservation = Some(waiter);
        drop(state);
        trace!("Acquire::poll for {:?} that reserved the semaphore", self);
        // No waiter can take a permit now.
        semaphore.reblock_if_unfair();
        Poll::Pending
    }
}

impl Future for Acquire<'_> {
    type Output = Result<(), AcquireError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        assert!(!self.completed);

        // One borrow of the semaphore state rather than two (`is_closed` and
        // `available_permits` each took their own). Both reads describe the same
        // instant, before the scheduling point below, so merging them is sound.
        // Reads *after* the switch must stay separate and fresh, because other
        // tasks may have run in between. A reserving acquire that will reserve
        // the semaphore changes its state as much as one that will succeed, so
        // it compares against the permits at which it reserves.
        let will_succeed = self.has_permits() || {
            let state = self.semaphore.state.borrow();
            state.closed || state.available() >= self.min_permits()
        };

        // If the acquire will succeed on the first try, we need to context switch once to allow the previous
        // event to become visible. If we won't succeed, then we still need to context switch if the act of
        // blocking does not commute with other operations on `batch_semaphore` (double-yield optimization,
        // reasoning below).
        //
        // Fair Semaphores: blocking adds the current task to an *ordered* waiter queue. Two blocking acquires
        // *do not commute* because in one ordering the queue will be [T1 T2] and in the other ordering [T2 T1].
        // Thus we cannot apply the double-yield optimization for fair semaphores.
        //
        // Unfair Semaphores: blocking adds the current task to an *unordered set* of waiters. To check if the
        // double-yield is valid we check if each operation (Z) on the semaphore commutes with a blocking acquire (Y1):
        //
        //     - Blocking Acquire: in both orderings `Z Y1` and `Y1 Z`, the waiter set has the same members, thus
        //       the operations commute.
        //     - Try Acquire: the try-acquire will fail in both orderings without changing the state of the semaphore
        //     - Release: if the release unblocks Y1, then the optimization is not applicable. Otherwise, it must
        //       unblock another task in the waiter set. As waiter-set insertion and removal for disjoint elements
        //       commutes, release operations also commute in this case.
        //
        // Thus we apply the double-yield optimization for *unfair* semaphores only, and not for an
        // unfair semaphore built `with_fair_releases`: its releases grant from the front of the
        // queue, so there too, two blocking acquires do not commute.
        let blocking_is_not_commutative = self.semaphore.fairness == Fairness::StrictlyFair
            || self.semaphore.release_fairness == Fairness::StrictlyFair;

        if self.never_polled && (will_succeed || blocking_is_not_commutative) {
            thread::switch();
        }
        self.never_polled = false;

        let out = if self.has_permits() {
            assert!(!self.is_queued());
            self.forget_grant();
            self.completed = true;
            trace!("Acquire::poll for {:?} with permits", self);
            Poll::Ready(Ok(()))
        } else if self.semaphore.is_closed() {
            assert!(!self.is_queued());
            self.completed = true;
            trace!("Acquire::poll for {:?} with closed", self);
            Poll::Ready(Err(AcquireError::closed()))
        } else if let Some(min_permits) = self.reserve_at {
            self.poll_reserving(min_permits, cx)
        } else {
            let is_queued = self.is_queued();
            trace!("Acquire::poll for {:?}; is queued: {is_queued:?}", self);

            // Sanity check: there should be a waker if the waiter is in
            // the queue. Also true for unfair semaphores, which wake by ref.
            //
            // `debug_assert` rather than `assert`: this takes a `std::sync::Mutex`
            // on every poll, including the uncontended fast path, purely to check
            // an internal invariant.
            debug_assert_eq!(
                is_queued,
                self.waiter
                    .as_ref()
                    .is_some_and(|waiter| waiter.waker.lock().unwrap().is_some())
            );

            // Should the waiter try to acquire permits here? Four cases:
            // 1. unfair semaphore, waiter not yet enqueued;
            // 2. fair semaphore, waiter not yet enqueued;
            // 3. unfair semaphore, waiter already enqueued.
            // 4. fair semaphore, waiter already enqueued;
            //
            // 1. and 2. are similar: the future was polled for the first time,
            // so the waiter will try to acquire some permits. If successful,
            // the waiter need not be enqueued, and the future is resolved.
            // Otherwise, the waiter is added to the queue.
            //
            // 3. is slightly different: the future was polled, even though the
            // waiter was already in the queue. This can happen either because
            // the semaphore just received some permits and woke the waiter up,
            // or because the future itself was polled manually. Either way,
            // the semaphore is queried.
            //
            // 4. is a case where we do not try to acquire permits. The request
            // would always fail, and the waiter should remain suspended until
            // the semaphore has explicitly unblocked it and given it permits
            // during a `release` call.
            let try_to_acquire = match (self.semaphore.fairness, is_queued) {
                // written this way to mirror the cases described above
                (Fairness::Unfair, false) | (Fairness::StrictlyFair, false) | (Fairness::Unfair, true) => true,
                (Fairness::StrictlyFair, true) => false,
            };

            if try_to_acquire {
                // Access the semaphore state directly instead of `try_acquire`,
                // because in case of `NoPermits`, we do not want to update the
                // clock, as this thread will be blocked below.
                let mut state = self.semaphore.state.borrow_mut();
                let id = state.id.unwrap();
                let acquire_result = state.acquire_permits(self.num_permits, self.semaphore.fairness, self.priority);
                drop(state);

                match acquire_result {
                    Ok(()) => {
                        if is_queued {
                            let waiter = self
                                .waiter
                                .clone()
                                .expect("a queued acquire must have an allocated waiter");
                            crate::annotations::record_semaphore_acquire_unblocked(
                                id,
                                waiter.task_id(),
                                waiter.num_permits,
                            );
                            self.semaphore.remove_waiter(&waiter);
                        } else {
                            crate::annotations::record_semaphore_acquire_fast(id, self.num_permits);
                        }
                        self.grant_permits();
                        self.completed = true;
                        trace!("Acquire::poll for {:?} that got permits", self);

                        // If the semaphore is unfair, re-block other waiting
                        // threads that can no longer succeed.
                        self.semaphore.reblock_if_unfair();

                        Poll::Ready(Ok(()))
                    }
                    Err(TryAcquireError::NoPermits) => {
                        // This acquire has to block, so it now becomes visible to
                        // whichever task releases permits. That is the first point
                        // at which shared state is needed, so it is where the
                        // `Waiter` gets allocated.
                        let waiter = self.waiter_for_blocking();

                        let mut maybe_waker = waiter.waker.lock().unwrap();
                        *maybe_waker = Some(cx.waker().clone());
                        drop(maybe_waker);

                        // Point the waiter at whoever is polling now: this future
                        // may have been created by a different task.
                        waiter.set_task_id(ExecutionState::me());

                        if !is_queued {
                            crate::annotations::record_semaphore_acquire_blocked(id, self.num_permits);
                            // `enqueue_waiter` sets `is_queued` itself.
                            self.semaphore.enqueue_waiter(&waiter, self.priority);
                        }
                        trace!("Acquire::poll for {:?} that is enqueued", self);
                        Poll::Pending
                    }
                    Err(TryAcquireError::Closed) => unreachable!(),
                }
            } else {
                // No progress made, future is still pending. The waiter stays in
                // the queue, but re-point it at the current poller and refresh
                // its waker: this future may have been created by (or last
                // polled by) another task, and `release` must wake whoever is
                // waiting now. Without this, a permit granted to this waiter
                // would unblock a task that is no longer interested, and the
                // actual poller would never be woken.
                let waiter = self
                    .waiter
                    .as_ref()
                    .expect("a queued acquire must have an allocated waiter");
                *waiter.waker.lock().unwrap() = Some(cx.waker().clone());
                waiter.set_task_id(ExecutionState::me());
                Poll::Pending
            }
        };
        if matches!(out, Poll::Pending) {
            // `Backtrace::capture()` is a noop (it returns the constant `disabled()`) if `RUST_BACKTRACE`/`RUST_LIB_BACKTRACE` is not set.
            ExecutionState::with(|state| {
                state.current_mut().backtrace = if backtrace_enabled() {
                    Some(std::backtrace::Backtrace::force_capture())
                } else {
                    None
                }
            })
        }
        out
    }
}

impl Drop for Acquire<'_> {
    fn drop(&mut self) {
        trace!("Acquire::drop for {:?}", self);
        if self.is_queued() {
            // If the associated waiter is in the wait list, remove it
            let waiter = self
                .waiter
                .clone()
                .expect("a queued acquire must have an allocated waiter");
            self.semaphore.remove_waiter(&waiter);
        } else if self.is_reserving() {
            // If the acquire holds the reservation, end it, so that it does not
            // keep the permits from other requests.
            let waiter = self
                .waiter
                .clone()
                .expect("a reserving acquire must have an allocated waiter");
            self.semaphore.cancel_reservation(&waiter);
        } else if self.has_permits() && !self.completed {
            // If the waiter was granted permits, release them. Note this must also
            // fire for an acquire that got its permits without ever allocating a
            // waiter, otherwise the semaphore leaks permits.
            self.forget_grant();
            self.semaphore.release(self.num_permits);
        }
    }
}

impl crate::annotations::WithName for &BatchSemaphore {
    fn with_name_and_kind(self, name: Option<&str>, kind: Option<&str>) -> Self {
        self.init_object_id();
        crate::annotations::record_name_for_object(self.state.borrow().id.unwrap(), name, kind);
        self
    }
}

impl crate::annotations::WithName for BatchSemaphore {
    fn with_name_and_kind(self, name: Option<&str>, kind: Option<&str>) -> Self {
        (&self).with_name_and_kind(name, kind);
        self
    }
}

impl BatchSemaphore {
    /// Returns a reference to this semaphore's resource signature.
    pub fn signature(&self) -> &ResourceSignature {
        &self.signature
    }
}
