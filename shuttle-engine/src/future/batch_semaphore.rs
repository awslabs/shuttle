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
    /// (see [`BatchSemaphore::acquire_reserving`]). Strictly fair semaphores ignore this.
    min_permits: usize,
    is_queued: AtomicBool,
    has_permits: AtomicBool,
    /// [`Fairness::ParkingLot`] only: a release took this waiter out of the queue and woke it to
    /// try again (`parking_lot`'s `TOKEN_NORMAL`). It is in `ParkingLotState::woken` until then.
    woken: AtomicBool,
    /// [`Fairness::ParkingLot`] only: the races that it lost, and the releases and reads that
    /// passed it while it was woken (see `BatchSemaphoreState::forget_stale_woken_and_pass`).
    overtakes: AtomicUsize,
    /// [`Fairness::ParkingLot`] only: the order of its first park, from 1 (0 until then).
    ticket: AtomicUsize,
    /// [`Fairness::ParkingLot`] only: the stand-in parked it again from another task (see
    /// `let_starving_woken_waiters_try`), and its `Acquire` has yet to report that park.
    park_unreported: AtomicBool,
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
            .field("woken", &self.woken)
            .field("overtakes", &self.overtakes)
            .field("ticket", &self.ticket)
            .field("park_unreported", &self.park_unreported)
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
            woken: AtomicBool::new(false),
            overtakes: AtomicUsize::new(0),
            ticket: AtomicUsize::new(0),
            park_unreported: AtomicBool::new(false),
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
        self.num_available += num_permits;
        self.permit_clocks.as_mut().unwrap().push_back((num_permits, clock));
    }
}

/// The state that only a [`Fairness::ParkingLot`] semaphore keeps. It is allocated on first use,
/// so that the semaphores of the other modes do not grow by it, and `const_new` can make one.
struct ParkingLotState {
    /// The waiters that a release woke to try again, and that have not tried yet (see
    /// `Waiter::woken`).
    ///
    /// A release takes the waiters that it wakes out of the queue, so that the queue holds exactly
    /// the parked requests, as `parking_lot`'s does, and `has_waiters` is `PARKED_BIT`. The
    /// eventual-fairness stand-in still needs them: every release and read that passes a woken
    /// waiter counts as one of its overtakes (`Waiter::overtakes`), and one that has been overtaken
    /// `Config::parking_lot_fairness_bound` times is let in, oldest request first. So a woken waiter
    /// stays here until its `Acquire` tries again or is dropped, or its task finishes.
    woken: Vec<Arc<Waiter>>,
    /// The ticket of the next request to park (see `Waiter::ticket`).
    next_ticket: usize,
    /// The join of the clocks of the releases, the reservations given up and the close, which
    /// [`BatchSemaphore::load_permits`] joins, so that a read is causally after every event that
    /// freed the permits it sees, as it is after every acquire (`PermitsAvailable::last_acquire`).
    ///
    /// `last_acquire` is not enough for that: a holder releases after it acquires, so a read that
    /// sees the permits free again would only be after the acquire. A replay up to the read's clock
    /// (see `ReplayScheduler::set_target_clock`) leaves out the events that are concurrent with that
    /// clock, so it could leave out the release, and the replayed read would see the permits still
    /// held and not reproduce the failure. Giving up a reservation and closing the semaphore change
    /// what a read sees without a release, so they count too. Like the clock of a Shuttle atomic,
    /// which joins every store, this joins every release, not only those whose permits are still
    /// free.
    last_release: VectorClock,
    /// The join of the clocks of the changes to the queue: the parks, and the releases, reads and
    /// drops that took waiters out of it. `parking_lot` changes its queue only under the queue's
    /// lock, so the reads of the queue join this.
    last_queue_change: VectorClock,
    /// The join of the clocks of the events that changed the woken waiters or their counts, which
    /// every release and read joins, so that the stand-in decides after what it counts.
    last_woken: VectorClock,
}

// Implement debug in order to not output the `VectorClock`s
impl fmt::Debug for ParkingLotState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParkingLotState")
            .field("woken", &self.woken)
            .field("next_ticket", &self.next_ticket)
            .finish()
    }
}

/// Fairness mode for the semaphore. Determines which threads are woken when
/// permits are released.
///
/// More modes may be added, so a `match` on a `Fairness` outside of Shuttle needs
/// a wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Fairness {
    /// The semaphore is strictly fair, so earlier requesters always get
    /// priority over later ones.
    StrictlyFair,

    /// The semaphore makes no guarantees about fairness. In particular,
    /// a waiter can be starved by other threads.
    Unfair,

    /// The semaphore behaves like the locks of the `parking_lot` crate, which are unfair, but
    /// eventually fair:
    ///
    /// * A request takes free permits even while others wait. One that cannot parks at the back of
    ///   a queue, and does not try again until a release wakes it.
    /// * [`BatchSemaphore::release`] takes the waiters that the free permits let in out of the
    ///   queue and wakes them, as an unlock of `parking_lot` unparks threads: from the front, it
    ///   passes over a waiter whose `min_permits` (see [`BatchSemaphore::acquire_reserving`]) are
    ///   not free, takes one whose are, which uses up its permits, and stops after one that can
    ///   only reserve the semaphore, or once no permit is left. With one permit, that is the front
    ///   waiter, as for a `Mutex`. With a permit for each reader of an `RwLock`, a majority for an
    ///   upgradable reader, and all of them for a writer, which reserves the lock as soon as no
    ///   other writer or upgradable reader holds it, it is the wake filter of the `RwLock`. The
    ///   permits stay free, so a woken waiter tries again like a new request, and parks again at
    ///   the back if it loses. [`BatchSemaphore::release_fair`] hands the permits to those waiters
    ///   instead, and [`BatchSemaphore::release_without_waking`] wakes none of them.
    /// * A request can reserve the semaphore (see [`BatchSemaphore::acquire_reserving`]), and an
    ///   upgrade does (see [`BatchSemaphore::upgrade`]).
    /// * `parking_lot` makes an unlock fair now and then, on a timer, and an OS soon runs a thread
    ///   that an unlock woke, while Shuttle's schedulers can starve a runnable task. In their
    ///   place, a woken request that has been overtaken
    ///   [`Config::parking_lot_fairness_bound`](crate::Config::parking_lot_fairness_bound) times,
    ///   by losing the race that a release woke it for, or by a release or a
    ///   [`BatchSemaphore::load_permits`] before it tried, tries at the next release or read, or at
    ///   another point that lets waiters in (a reservation given up, the permits that the holder of
    ///   a reservation leaves free when it takes its own, or a woken [`Acquire`] dropped), oldest
    ///   request first. While another request holds the reservation, only a read lets it try. It is
    ///   granted the permits, or the reservation, if its `min_permits` are free and no other
    ///   request holds the reservation, and parks again at the back otherwise. So no loop of
    ///   releases or reads can starve a request. For a task that waits in a blocking call, as the
    ///   locks of `shuttle-parking_lot` do, this is the schedule of `parking_lot` in which the
    ///   woken thread tries right after the release, so it only leaves schedules out. An async task
    ///   that keeps a woken [`Acquire`] pending while it waits for something else could not take
    ///   the permits then, so for it a grant can add an outcome, in which the `Acquire` holds the
    ///   permits until the task polls it again, and a deadlock that this causes at one bound can
    ///   disappear at a larger one.
    ///
    /// As on a strictly fair semaphore, the order of the parks decides which waiters a release
    /// wakes, so Shuttle explores every order in which they park. A waiter that a release or a
    /// read wakes or grants is causally after it, as a thread that an unlock unparks is after the
    /// unlock.
    ParkingLot,
}

/// What a release does to the waiters of a [`Fairness::ParkingLot`] semaphore: wake them
/// ([`BatchSemaphore::release`]), hand them the permits ([`BatchSemaphore::release_fair`]), or
/// wake nobody in the queue ([`BatchSemaphore::release_without_waking`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wake {
    Plain,
    HandOff,
    NotQueue,
}

/// Where an acquire request sits relative to waiters that are already queued on
/// a [`Fairness::StrictlyFair`] semaphore. Ignored by the other modes, on which
/// a request takes free permits regardless of the queue.
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
    // (1) if the semaphore is strictly fair, `waiters` is nonempty and the head
    // waiter is `H`, then `H.num_permits > permits_available.available()`.  (In
    // other words, we are never in a state where there are enough permits
    // available for the first waiter.  This invariant is ensured by the `drop`
    // handler below.)
    //
    // (2) W is in waiters iff W.is_queued
    //
    // (3) W.is_queued ==> !W.has_permits
    // Note: the converse is not true.  We can have !W.has_permits && !W.is_queued
    // when the Acquire is created but not yet polled.
    //
    // (4) closed ==> waiters.is_empty(), and no waiter is woken
    //
    // (5) if `reservation` is `Some(R)`, then the semaphore is not strictly fair,
    // and !R.is_queued && !R.has_permits
    //
    // (6) closed ==> reservation.is_none()
    //
    // (7) W is in `parking_lot.woken` iff W.woken, and then the semaphore is a
    // ParkingLot one, and W is neither queued nor granted nor the reservation's.
    // `parking_lot` is `None` on the other modes.
    waiters: VecDeque<Arc<Waiter>>,
    /// The waiter that holds the semaphore's reservation, if any (see
    /// [`BatchSemaphore::acquire_reserving`]). While it is set, the available
    /// permits are kept for this waiter: no other request can take one, and the
    /// waiter takes its `num_permits` as soon as that many are available.
    reservation: Option<Arc<Waiter>>,
    permits_available: PermitsAvailable,
    // TODO: should there be a clock for the close event?
    closed: bool,
    /// Allocated by the method of the same name.
    parking_lot: Option<Box<ParkingLotState>>,
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
        } else if self.waiters.is_empty() || fairness != Fairness::StrictlyFair || priority == Priority::Front {
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

    fn unblock_waiters_from_front(&mut self) {
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
            } else {
                return;
            }
        }
    }

    /// The state that only a [`Fairness::ParkingLot`] semaphore keeps.
    fn parking_lot(&mut self) -> &mut ParkingLotState {
        self.parking_lot.get_or_insert_with(|| {
            Box::new(ParkingLotState {
                woken: Vec::new(),
                next_ticket: 1,
                last_release: VectorClock::new(),
                last_queue_change: VectorClock::new(),
                last_woken: VectorClock::new(),
            })
        })
    }

    /// Take `waiter` out of the woken waiters, if a release woke it, because its `Acquire` tries
    /// again now, or is dropped. Returns whether it was woken.
    fn forget_woken(&mut self, waiter: Option<&Arc<Waiter>>) -> bool {
        let (Some(waiter), Some(parking_lot)) = (waiter, self.parking_lot.as_mut()) else {
            return false;
        };
        if !waiter.woken.swap(false, Ordering::SeqCst) {
            return false;
        }
        let index = parking_lot.woken.iter().position(|w| Arc::ptr_eq(w, waiter));
        parking_lot.woken.remove(index.expect("a woken waiter is in `woken`"));
        waiter.waker.lock().unwrap().take();
        parking_lot.last_woken.update(&current::clock());
        true
    }

    /// What a release, a read and the other points that let waiters into a [`Fairness::ParkingLot`]
    /// semaphore do first: join the changes to the woken waiters, forget those whose task has
    /// finished, and if `passes`, as a release and a read do, overtake the others (see
    /// `Waiter::overtakes`).
    fn forget_stale_woken_and_pass(&mut self, passes: bool) {
        let parking_lot = self.parking_lot();
        // The stand-in decides on the woken waiters and their counts, so it is after the events
        // that changed them, and a replay up to here counts the same. It also decides on the free
        // permits and the reservation, which it is after only through the events that it took part
        // in: as for a `try_acquire`, a replay that leaves out a release or an acquire of another
        // task can decide differently.
        ExecutionState::with(|s| s.update_clock(&parking_lot.last_woken));
        parking_lot.woken.retain(|waiter| {
            let stale = is_stale(waiter);
            if stale {
                trace!("dropping stale woken waiter {:?} for finished task", waiter);
                waiter.woken.store(false, Ordering::SeqCst);
                waiter.waker.lock().unwrap().take();
            }
            !stale
        });
        if passes && !parking_lot.woken.is_empty() {
            for waiter in &parking_lot.woken {
                waiter.overtakes.fetch_add(1, Ordering::SeqCst);
            }
            parking_lot.last_woken.update(&current::clock());
        }
    }

    /// What a [`Fairness::ParkingLot`] semaphore does to its waiters once a release has put its
    /// permits back (`is_release`), and at the other points that let waiters in: a reservation
    /// given up, a holder that takes its permits and leaves some free, and a woken `Acquire`
    /// dropped.
    #[inline(never)]
    fn wake_parking_lot(&mut self, mut wake: Wake, is_release: bool) {
        if is_release {
            // A release changes what `load_permits` reads.
            let clock = current::clock();
            self.parking_lot().last_release.update(&clock);
        }
        self.forget_stale_woken_and_pass(is_release);
        if let Some(holder) = &self.reservation {
            // While a reservation holds the semaphore (`WRITER_BIT`), only its holder can make
            // progress, once all of its permits are free, as the last reader to leave wakes a
            // writer that waits for the readers. A stale holder is dropped, as in
            // `wake_unfair_waiters` (`load_permits` is after that through `last_release` or
            // `last_woken`), and then the waiters that it kept out are let in, as a writer that
            // gives up `WRITER_BIT` lets them in, even by a release that wakes nobody in the queue.
            if !is_stale(holder) {
                if holder.num_permits <= self.permits_available.available() {
                    wake_task(holder, false);
                }
                return;
            }
            trace!("dropping stale reservation {:?} for finished task", holder);
            self.reservation = None;
            if wake == Wake::NotQueue {
                wake = Wake::Plain;
            }
        }
        if wake != Wake::NotQueue && self.waiters.iter().any(|waiter| !is_stale(waiter)) {
            // The release reads the queue, as an unlock that sees `PARKED_BIT` unparks threads.
            let parking_lot = self.parking_lot();
            ExecutionState::with(|s| s.update_clock(&parking_lot.last_queue_change));
            let unparked = self.take_unpark_set();
            if !unparked.is_empty() {
                self.parking_lot().last_queue_change.update(&current::clock());
            }
            let woke = wake == Wake::Plain && !unparked.is_empty();
            for waiter in unparked {
                if wake == Wake::HandOff {
                    self.grant_or_reserve(waiter);
                } else {
                    // `TOKEN_NORMAL`: the permits stay free, and the waiter tries again like a new
                    // request. It keeps its waker, for a release that grants it.
                    trace!("woke waiter {:?} to try again", waiter);
                    waiter.woken.store(true, Ordering::SeqCst);
                    wake_task(&waiter, false);
                    self.parking_lot().woken.push(waiter);
                }
            }
            if woke {
                self.parking_lot().last_woken.update(&current::clock());
            }
        }
        // As woken threads try after the unlock.
        self.let_starving_woken_waiters_try();
    }

    /// The eventual-fairness stand-in (see [`Fairness::ParkingLot`]): each woken waiter that has
    /// been overtaken `parking_lot_fairness_bound` times tries now, oldest request first. It is
    /// granted its permits or the reservation if its `min_permits` are free and no other request
    /// holds the reservation, and otherwise it lost, and parks again at the back, keeping its
    /// waker. Serving the oldest request first keeps it from losing every grant to requests woken
    /// before it, and parking it keeps tasks that hand the permits to each other with
    /// `release_fair` from starving it: a release lets it in in its turn, and grants it.
    fn let_starving_woken_waiters_try(&mut self) {
        let bound = ExecutionState::with(|s| s.config.parking_lot_fairness_bound);
        let woken = &mut self.parking_lot().woken;
        woken.sort_by_key(|waiter| waiter.ticket.load(Ordering::SeqCst));
        let starving = woken
            .extract_if(.., |waiter| waiter.overtakes.load(Ordering::SeqCst) >= bound)
            .collect::<Vec<_>>();
        let tried = !starving.is_empty();
        for waiter in starving {
            waiter.woken.store(false, Ordering::SeqCst);
            if self.reservation.is_none() && waiter.min_permits <= self.permits_available.available() {
                trace!("granting woken waiter {:?}, which was overtaken too often", waiter);
                self.grant_or_reserve(waiter);
            } else {
                trace!("woken waiter {:?}, which was overtaken too often, parks again", waiter);
                waiter.overtakes.fetch_add(1, Ordering::SeqCst);
                waiter.park_unreported.store(true, Ordering::SeqCst);
                assert!(!waiter.is_queued.swap(true, Ordering::SeqCst));
                self.waiters.push_back(waiter);
                self.parking_lot().last_queue_change.update(&current::clock());
            }
        }
        if tried {
            self.parking_lot().last_woken.update(&current::clock());
        }
    }

    /// Take the waiters that the free permits let in out of the queue, as `parking_lot`'s
    /// `unpark_one` (`Mutex`) and the filter of `wake_parked_threads` (`RwLock`) unpark threads.
    /// From the front, a waiter whose `min_permits` are not free stays (`FilterOp::Skip`). One
    /// whose are is taken, and uses up its `num_permits`, unless they are not all free: then it
    /// would reserve the semaphore, as a writer sets `WRITER_BIT` and waits for the readers, and it
    /// ends the walk, as does one that takes the last free permit (`FilterOp::Stop`). Stale waiters
    /// are dropped (see `is_stale`).
    fn take_unpark_set(&mut self) -> Vec<Arc<Waiter>> {
        let mut free = self.permits_available.available();
        let mut unparked = Vec::new();
        let mut skipped = Vec::new();
        while let Some(waiter) = self.waiters.pop_front() {
            if is_stale(&waiter) {
                waiter.is_queued.store(false, Ordering::SeqCst);
                // As in `unblock_waiters_from_front`.
                waiter.waker.lock().unwrap().take();
                trace!("dropping stale waiter {:?} for finished task", waiter);
            } else if waiter.min_permits > free {
                skipped.push(waiter);
            } else {
                assert!(waiter.is_queued.swap(false, Ordering::SeqCst));
                // One that would reserve the semaphore leaves no permit for the rest either.
                free = free.saturating_sub(waiter.num_permits);
                unparked.push(waiter);
                if free == 0 {
                    break;
                }
            }
        }
        for waiter in skipped.into_iter().rev() {
            self.waiters.push_front(waiter);
        }
        unparked
    }

    /// Give `waiter`, which is neither queued nor woken, all of its permits if they are free
    /// (`TOKEN_HANDOFF`), and otherwise the reservation, as `parking_lot` hands a writer
    /// `WRITER_BIT` while readers hold the lock.
    fn grant_or_reserve(&mut self, waiter: Arc<Waiter>) {
        assert!(!waiter.is_queued.load(Ordering::SeqCst));
        // A request that the permits or the reservation refuse is after this (see `try_acquire`).
        self.permits_available.last_acquire.update(&current::clock());
        if waiter.num_permits > self.permits_available.available() {
            assert!(self.reservation.is_none());
            trace!("handed the reservation to waiter {:?}", waiter);
            self.permits_available.last_acquire.update(&waiter.clock);
            self.reservation = Some(waiter);
            return;
        }
        crate::annotations::record_semaphore_acquire_unblocked(self.id.unwrap(), waiter.task_id(), waiter.num_permits);
        // As in `unblock_waiters_from_front`.
        let clock = self
            .permits_available
            .acquire(waiter.num_permits, waiter.clock.clone())
            .unwrap();
        trace!("granted {:?} permits to waiter {:?}", waiter.num_permits, waiter);
        assert!(!waiter.has_permits.swap(true, Ordering::SeqCst));
        ExecutionState::with(|s| {
            let task = s.get_mut(waiter.task_id());
            assert!(!task.finished());
            task.clock.update(&clock);
        });
        wake_task(&waiter, true);
    }

    /// A [`Fairness::ParkingLot`] semaphore closes: the woken waiters are no longer woken, and
    /// `load_permits` is after the close. Returns the woken waiters.
    #[cold]
    fn close_parking_lot(&mut self) -> Vec<Arc<Waiter>> {
        let parking_lot = self.parking_lot();
        parking_lot.last_release.update(&current::clock());
        let woken = std::mem::take(&mut parking_lot.woken);
        for waiter in &woken {
            assert!(waiter.woken.swap(false, Ordering::SeqCst));
        }
        woken
    }
}

/// Wake the task that polled `waiter`'s `Acquire` last, taking the waker out of the waiter if
/// `take`. A [`Fairness::ParkingLot`] semaphore never blocks a task or puts it to sleep itself: it
/// wakes the waiters that it lets in with this. The task is causally after the current one, as a
/// thread is after the unlock that unparks it: the permits that it takes can be those of an older
/// release, which stayed free while it was parked. A task that sleeps on its own waker, as in
/// `block_on`, is made runnable directly, as calling the waker would also mark it woken, and keep
/// it runnable for one more, useless poll.
fn wake_task(waiter: &Waiter, take: bool) {
    let mut slot = waiter.waker.lock().unwrap();
    let waker = if take { slot.take() } else { slot.clone() };
    drop(slot);
    let woken = ExecutionState::with(|s| {
        let waker_clock = s.try_current().map(|task| task.clock.clone());
        let task = s.get_mut(waiter.task_id());
        if task.finished() {
            return true;
        }
        if let Some(clock) = &waker_clock {
            task.clock.update(clock);
        }
        match &waker {
            Some(waker) if task.sleeping() && waker.will_wake(&task.waker()) => {
                task.unblock();
                true
            }
            Some(_) => false,
            None => true,
        }
    });
    // Outside of `with`: Shuttle's wakers use the execution state.
    if let (false, Some(waker)) = (woken, waker) {
        waker.wake();
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
            reservation: None,
            permits_available: PermitsAvailable::new(num_permits),
            closed: false,
            parking_lot: None,
        });
        Self {
            state,
            fairness,
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
            reservation: None,
            permits_available: PermitsAvailable::const_new(num_permits),
            closed: false,
            parking_lot: None,
        });
        Self {
            state,
            fairness,
            signature,
        }
    }

    /// Returns the current number of available permits. While a reservation
    /// holds the semaphore (see [`BatchSemaphore::acquire_reserving`]), this is
    /// zero: the available permits are kept for the holder.
    pub fn available_permits(&self) -> usize {
        let state = self.state.borrow();
        state.available()
    }

    /// Reads the semaphore's state in one scheduling point: `None` if it is closed, and otherwise
    /// `Some` of the free permits, which are zero while a reservation keeps them (see
    /// [`BatchSemaphore::available_permits`]).
    ///
    /// This models a load of a lock's atomic word, like `parking_lot`'s `is_locked`: the read takes
    /// and releases nothing, while probing with `try_acquire` and `release` would hold the permits
    /// across more scheduling points, where other tasks could see a state that the lock never
    /// shows. The read is causally after the acquires and the reservations before it, as a failed
    /// `try_acquire` is. On a [`Fairness::ParkingLot`] semaphore, it is also after the releases,
    /// the reservations given up and the close, so that a replay up to its clock (see
    /// `ReplayScheduler::set_target_clock`) reads the same state, and it overtakes the woken
    /// waiters as a release does, so that a task that reads the state until one of them makes
    /// progress cannot starve it. The other modes do not keep the clocks of their releases, so
    /// that their releases do not pay for them.
    pub fn load_permits(&self) -> Option<usize> {
        thread::switch();

        let mut state = self.state.borrow_mut();
        if state.parking_lot.is_some() {
            state.forget_stale_woken_and_pass(true);
            state.let_starving_woken_waiters_try();
        }
        ExecutionState::with(|s| {
            s.update_clock(&state.permits_available.last_acquire);
            if let Some(parking_lot) = &state.parking_lot {
                s.update_clock(&parking_lot.last_release);
            }
        });
        if state.closed {
            None
        } else {
            Some(state.available())
        }
    }

    /// Whether a request waits in the semaphore's queue, without a scheduling point, so that it
    /// can be read right after [`BatchSemaphore::load_permits`] to see the same instant. Neither a
    /// request whose task has finished without dropping its [`Acquire`] counts, nor the holder of
    /// the reservation, which is not in the queue. A request that a release woke counts on an
    /// unfair semaphore, where it stays in the queue until it tries again.
    ///
    /// This models `parking_lot`'s `PARKED_BIT`, which its locks' `bump` methods read. On a
    /// [`Fairness::ParkingLot`] semaphore, it is exactly that bit, and when it is true, it is
    /// causally after the changes to the queue, as a load of the bit is after the parks and
    /// unparks that left it set. The other modes do not keep the clocks of their queues, so that
    /// their requests do not pay for them.
    pub fn has_waiters(&self) -> bool {
        let state = self.state.borrow();
        let waiting = state.waiters.iter().any(|waiter| !is_stale(waiter));
        if let (true, Some(parking_lot)) = (waiting, &state.parking_lot) {
            // Like `is_stale`, don't insist on a running task.
            let _ = ExecutionState::try_with(|s| {
                if s.try_current().is_some() {
                    s.update_clock(&parking_lot.last_queue_change);
                }
            });
        }
        waiting
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
        // On a ParkingLot semaphore, the woken waiters wait too.
        let woken = if self.fairness == Fairness::ParkingLot {
            state.close_parking_lot()
        } else {
            Vec::new()
        };

        // Wake up all the waiters, and the holder of the reservation, which waits
        // too.  Since we've marked the state as closed, they will all return
        // `AcquireError::closed` from their acquire calls.
        let ptr = &*state as *const BatchSemaphoreState;
        let holder = state.reservation.take();
        let queued = state
            .waiters
            .drain(..)
            .inspect(|waiter| assert!(waiter.is_queued.swap(false, Ordering::SeqCst)));
        for waiter in queued.chain(woken).chain(holder) {
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

    /// As [`BatchSemaphore::try_acquire`], but also fails while a request waits in the queue (see
    /// [`BatchSemaphore::has_waiters`]), even if the permits are free. The motivating use case is
    /// `parking_lot`'s `try_write`, a compare-and-swap of the lock word from zero, which fails
    /// while `PARKED_BIT` is set. On a [`Fairness::ParkingLot`] semaphore, a failure is also
    /// causally after the changes to the queue, as for `has_waiters`.
    pub fn try_acquire_without_overtaking(&self, num_permits: usize) -> Result<(), TryAcquireError> {
        thread::switch();

        self.init_object_id();
        let mut state = self.state.borrow_mut();
        let id = state.id.unwrap();
        let res = if state.closed {
            Err(TryAcquireError::Closed)
        } else if state.waiters.iter().any(|waiter| !is_stale(waiter)) {
            Err(TryAcquireError::NoPermits)
        } else {
            state.acquire_permits(num_permits, self.fairness, Priority::Back)
        };
        if res.is_err() {
            ExecutionState::with(|s| {
                s.update_clock(&state.permits_available.last_acquire);
                if let Some(parking_lot) = &state.parking_lot {
                    s.update_clock(&parking_lot.last_queue_change);
                }
            });
        }
        drop(state);

        if res.is_ok() {
            self.reblock_if_unfair();
        }

        crate::annotations::record_semaphore_try_acquire(id, num_permits, res.is_ok());

        res
    }

    /// Clean-up method used when a thread succeeds in acquiring permits. If
    /// the semaphore is unfair, a preceding `release` may have unblocked a
    /// number of threads, some of which may no longer be able to succeed with
    /// the permits remaining in the semaphore.
    fn reblock_if_unfair(&self) {
        if self.fairness == Fairness::Unfair {
            let state = self.state.borrow_mut();
            ExecutionState::with(|s| {
                let me = s.try_current().map(|task| task.id());
                for waiter in &state.waiters {
                    let available = state.permits_available.available();
                    // A queued waiter cannot make progress while a reservation
                    // keeps the available permits.
                    let can_progress = state.reservation.is_none() && waiter.min_permits <= available;
                    // Skip stale waiters (see `is_stale`): there is nobody to
                    // block. And skip the current task's own waiters: it is
                    // running, which an `Acquire` of its that is still queued
                    // doesn't change, and it would only block itself.
                    let task = waiter.task_id();
                    if !can_progress && Some(task) != me && s.try_get(task).is_some_and(|t| !t.finished()) {
                        // Block this waiter: it cannot succeed (there are not
                        // enough permits available); its `poll` would return
                        // without resolving.
                        s.get_mut(task).block(false);
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

        if self.fairness == Fairness::ParkingLot {
            // The `Acquire` reports this park itself (see `Waiter::park_unreported`).
            let parking_lot = state.parking_lot();
            parking_lot.last_queue_change.update(&current::clock());
            if waiter.ticket.load(Ordering::SeqCst) == 0 {
                waiter.ticket.store(parking_lot.next_ticket, Ordering::SeqCst);
                parking_lot.next_ticket += 1;
            }
            waiter.park_unreported.store(false, Ordering::SeqCst);
        }
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
                    state.unblock_waiters_from_front();
                }
            }
            Fairness::Unfair => {}
            // A parked `parking_lot` thread whose timed lock times out unparks nobody either.
            Fairness::ParkingLot => {
                let clock = current::clock();
                state.parking_lot().last_queue_change.update(&clock);
            }
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
        if self.fairness == Fairness::ParkingLot {
            // It changes what `load_permits` reads.
            state.parking_lot().last_release.update(&current::clock());
        }

        // Wake the waiters that can now take the permits, unless `release`
        // wouldn't either (see `ExecutionState::should_stop`).
        let can_wake = ExecutionState::try_with(|s| !s.stops(std::thread::panicking())).unwrap_or(false);
        if can_wake {
            match self.fairness {
                Fairness::Unfair => state.wake_unfair_waiters(),
                // As a writer that gives up `WRITER_BIT` unparks threads, without a hand-off.
                Fairness::ParkingLot => state.wake_parking_lot(Wake::Plain, false),
                Fairness::StrictlyFair => unreachable!("a strictly fair semaphore has no reservations"),
            }
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
    /// an unfair or a [`Fairness::ParkingLot`] semaphore supports this.
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
    /// On a [`Fairness::ParkingLot`] semaphore, a parked reserving request is
    /// woken and tries again like any other, and a
    /// [`BatchSemaphore::release_fair`] that lets it in hands it the
    /// reservation, as `parking_lot` hands a writer `WRITER_BIT`.
    ///
    /// # Panics
    ///
    /// Panics if the semaphore is strictly fair (its queue already keeps the
    /// permits for its first waiter), if `num_permits` is zero, or if
    /// `min_permits > num_permits`.
    pub fn acquire_reserving(&self, min_permits: usize, num_permits: usize) -> Acquire<'_> {
        assert!(
            self.fairness != Fairness::StrictlyFair,
            "only an unfair semaphore supports reservations"
        );
        assert!(num_permits > 0);
        assert!(min_permits <= num_permits);

        self.init_object_id();
        Acquire::new_reserving(self, num_permits, min_permits)
    }

    /// Release `num_permits` back to the Semaphore. On a [`Fairness::ParkingLot`] semaphore, this
    /// wakes the waiters that the permits let in, as `parking_lot`'s plain unlocks do.
    pub fn release(&self, num_permits: usize) {
        self.release_with(num_permits, Wake::Plain);
    }

    /// [`BatchSemaphore::release`], but on a [`Fairness::ParkingLot`] semaphore, hand the permits
    /// to the waiters that they let in, inside the release, and the reservation to one that they
    /// let reserve but not take all of its permits, as the `unlock_fair` of `parking_lot`'s
    /// `MutexGuard` and `RwLockWriteGuard`, and the `bump` of those and of
    /// `RwLockUpgradableReadGuard`, do. In `parking_lot` 0.12.5,
    /// `RwLockUpgradableReadGuard::unlock_fair` is a plain unlock, a `release`, and the fair unlock
    /// and `bump` of `RwLockReadGuard` are a [`BatchSemaphore::release_without_waking`].
    pub fn release_fair(&self, num_permits: usize) {
        self.release_with(num_permits, Wake::HandOff);
    }

    /// [`BatchSemaphore::release`], but on a [`Fairness::ParkingLot`] semaphore, wake no waiter in
    /// the queue, as `parking_lot`'s `unlock_shared` wakes only a writer that waits for the readers
    /// to leave: this wakes the holder of the reservation once all of its permits are free. If the
    /// holder's task has finished, it drops the reservation and wakes the queue as `release` does,
    /// and while no request holds the reservation, it lets a woken waiter that has been overtaken
    /// `parking_lot_fairness_bound` times try, like any release (see [`Fairness::ParkingLot`]).
    pub fn release_without_waking(&self, num_permits: usize) {
        self.release_with(num_permits, Wake::NotQueue);
    }

    #[inline]
    fn release_with(&self, num_permits: usize, wake: Wake) {
        // Execution teardown can unwind a task's stack from this scheduling point, which is often in
        // a destructor that releases a lock (see `ExecutionState::tear_down`). The permits must not
        // be lost then, as destructors that run later can need them.
        struct ReleaseOnUnwind<'a>(&'a BatchSemaphore, usize, Wake);
        impl Drop for ReleaseOnUnwind<'_> {
            fn drop(&mut self) {
                self.0.release_no_scheduling_point(self.1, self.2);
            }
        }
        let release_on_unwind = ReleaseOnUnwind(self, num_permits, wake);
        thread::switch();
        std::mem::forget(release_on_unwind);

        self.release_no_scheduling_point(num_permits, wake);
    }

    /// `release_with` without its scheduling point.
    #[inline]
    fn release_no_scheduling_point(&self, num_permits: usize, wake: Wake) {
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
            if self.fairness == Fairness::ParkingLot {
                for waiter in state.close_parking_lot() {
                    waiter.waker.lock().unwrap().take();
                }
            }
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

        match self.fairness {
            Fairness::StrictlyFair => {
                // in a strictly fair mode we will grant permits to waiters from the front
                // of the queue, as long as there are enough permits available
                state.unblock_waiters_from_front();
            }
            Fairness::Unfair => {
                // in an unfair mode, we will unblock all the waiters for which
                // there are enough permits available, then let them race
                state.wake_unfair_waiters();
            }
            Fairness::ParkingLot => {
                // in a ParkingLot mode, we will wake the waiters that the permits
                // let in, or hand the permits to them, as `wake` says
                state.wake_parking_lot(wake, true);
            }
        }
        drop(state);
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
    /// An unfair or a [`Fairness::ParkingLot`] semaphore lets requests overtake its queue anyway,
    /// so there the upgrade reserves the semaphore instead (see
    /// [`BatchSemaphore::acquire_reserving`]), at once unless another request holds the
    /// reservation. From then on, no other request can take a permit, so the upgrade again waits
    /// only for the tasks that hold permits, and nothing can overtake it.
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
            Fairness::Unfair | Fairness::ParkingLot => Acquire::new_reserving(self, num_permits, 0),
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

// Safety: Semaphore is never actually passed across true threads, only across continuations. The
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

    /// Did a release of a [`Fairness::ParkingLot`] semaphore wake this acquire to try again, which
    /// it has not done yet (see `Waiter::woken`)?
    fn is_woken(&self) -> bool {
        self.waiter
            .as_ref()
            .is_some_and(|waiter| waiter.woken.load(Ordering::SeqCst))
    }

    /// Take this acquire's waiter, parked on a [`Fairness::ParkingLot`] semaphore by a task that
    /// has finished (see `is_stale`), out of the queue, as a `parking_lot` thread whose timed lock
    /// times out leaves it, so that the task that polls it now tries like a new request: it waited
    /// for nobody, so a release may have passed it over. Returns whether it was stale.
    fn leave_queue_if_stale(&self) -> bool {
        let waiter = self
            .waiter
            .as_ref()
            .expect("a queued acquire must have an allocated waiter");
        if !is_stale(waiter) {
            return false;
        }
        trace!("taking stale waiter {:?} out of the queue for its new poller", waiter);
        self.semaphore.remove_waiter(waiter);
        waiter.waker.lock().unwrap().take();
        true
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
    /// permits yet and that the semaphore is open. A strictly fair semaphore has
    /// no reserving acquires.
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
        let was_woken = semaphore.fairness == Fairness::ParkingLot && state.forget_woken(self.waiter.as_ref());

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
            if semaphore.fairness != Fairness::ParkingLot {
                // Let the waiters race for any permits that are left.
                state.wake_unfair_waiters();
            } else if state.permits_available.available() > 0 {
                // Wake the waiters that the permits that are left let in.
                state.wake_parking_lot(Wake::Plain, false);
            }
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
            if was_woken {
                // It lost the race that a release woke it for (see `Waiter::overtakes`).
                waiter.overtakes.fetch_add(1, Ordering::SeqCst);
            }
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
            } else if was_woken {
                crate::annotations::record_semaphore_acquire_unblocked(id, ExecutionState::me(), self.num_permits);
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
        // ParkingLot Semaphores: the same holds, as a release wakes waiters from the front of the queue.
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
        // Thus we apply the double-yield optimization for *unfair* semaphores only
        let blocking_is_not_commutative = self.semaphore.fairness != Fairness::Unfair;

        if self.never_polled && (will_succeed || blocking_is_not_commutative) {
            thread::switch();
        }
        self.never_polled = false;

        // Takes a waiter that a finished task parked out of the queue (see `leave_queue_if_stale`).
        // A queued waiter has no permits, and its semaphore is open (invariants (3) and (4)).
        let parked =
            self.semaphore.fairness == Fairness::ParkingLot && self.is_queued() && !self.leave_queue_if_stale();
        let out = if self.has_permits() {
            assert!(!self.is_queued());
            self.completed = true;
            trace!("Acquire::poll for {:?} with permits", self);
            Poll::Ready(Ok(()))
        } else if self.semaphore.is_closed() {
            assert!(!self.is_queued());
            self.completed = true;
            trace!("Acquire::poll for {:?} with closed", self);
            Poll::Ready(Err(AcquireError::closed()))
        } else if parked {
            // A parked waiter does not try: a release that lets it in takes it out of the queue,
            // and grants or wakes it. As on a fair semaphore, follow the current poller.
            trace!("Acquire::poll for parked {:?}", self);
            let waiter = self.waiter.as_ref().unwrap();
            debug_assert!(waiter.waker.lock().unwrap().is_some());
            *waiter.waker.lock().unwrap() = Some(cx.waker().clone());
            waiter.set_task_id(ExecutionState::me());
            if waiter.park_unreported.swap(false, Ordering::SeqCst) {
                let id = self.semaphore.state.borrow().id.unwrap();
                crate::annotations::record_semaphore_acquire_blocked(id, self.num_permits);
            }
            Poll::Pending
        } else if let Some(min_permits) = self.reserve_at {
            self.poll_reserving(min_permits, cx)
        } else {
            let is_queued = self.is_queued();
            trace!("Acquire::poll for {:?}; is queued: {is_queued:?}", self);

            // Sanity check: there should be a waker if the waiter is in
            // the queue. Also true for unfair semaphores, which wake by ref,
            // and for woken waiters, which keep theirs.
            //
            // `debug_assert` rather than `assert`: this takes a `std::sync::Mutex`
            // on every poll, including the uncontended fast path, purely to check
            // an internal invariant.
            debug_assert_eq!(
                is_queued || self.is_woken(),
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
                // A woken waiter tries like a new one, and a parked one is polled above.
                (Fairness::ParkingLot, _) => true,
            };

            if try_to_acquire {
                // Access the semaphore state directly instead of `try_acquire`,
                // because in case of `NoPermits`, we do not want to update the
                // clock, as this thread will be blocked below.
                let mut state = self.semaphore.state.borrow_mut();
                let id = state.id.unwrap();
                let was_woken =
                    self.semaphore.fairness == Fairness::ParkingLot && state.forget_woken(self.waiter.as_ref());
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
                        } else if was_woken {
                            let me = ExecutionState::me();
                            crate::annotations::record_semaphore_acquire_unblocked(id, me, self.num_permits);
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

                        if was_woken {
                            // It lost the race that a release woke it for.
                            waiter.overtakes.fetch_add(1, Ordering::SeqCst);
                        }
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
            self.semaphore.release(self.num_permits);
        } else if self.semaphore.fairness == Fairness::ParkingLot && self.is_woken() {
            // If a release woke the acquire to try again, and it never did, wake
            // the waiters that the free permits let in in its place, unless
            // `release` wouldn't either (see `ExecutionState::should_stop`).
            trace!("passing on the wake of {:?}", self);
            let mut state = self.semaphore.state.borrow_mut();
            assert!(state.forget_woken(self.waiter.as_ref()));
            if ExecutionState::try_with(|s| !s.stops(std::thread::panicking())).unwrap_or(false) {
                state.wake_parking_lot(Wake::Plain, false);
            }
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

#[cfg(test)]
mod parking_lot_filter_tests {
    //! `take_unpark_set` takes exactly the waiters that `parking_lot` 0.12.5's unlocks unpark: the
    //! filter of `RawRwLock::wake_parked_threads`, which this copies line by line, for every queue
    //! of up to 8 parked requests and every release that unparks threads, and `unpark_one` of
    //! `RawMutex`. A reader of the `RwLock` takes one permit, an upgradable reader a majority, and
    //! a writer all of them, reserving the semaphore once no other writer or upgradable reader
    //! holds it.
    use super::*;

    const N: usize = 1 << 30;
    const UPGRADABLE: usize = N / 2 + 1;
    const WRITER_RESERVES_AT: usize = N - UPGRADABLE + 1;

    // `parking_lot`'s state bits, and the tokens that its requests park with.
    const UPGRADABLE_BIT: usize = 0b0100;
    const WRITER_BIT: usize = 0b1000;
    const ONE_READER: usize = 0b10000;

    #[derive(Clone, Copy, Debug)]
    enum Request {
        Read,
        UpgradableRead,
        Write,
    }

    /// `wake_parked_threads`' filter, from `parking_lot`'s initial state `state`: which requests
    /// of `queue` it unparks.
    fn parking_lot_filter(queue: &[Request], mut state: usize) -> Vec<bool> {
        let mut unparked = vec![false; queue.len()];
        for (i, request) in queue.iter().enumerate() {
            let token = match request {
                Request::Read => ONE_READER,
                Request::UpgradableRead => ONE_READER | UPGRADABLE_BIT,
                Request::Write => WRITER_BIT,
            };
            if state & WRITER_BIT != 0 {
                break; // FilterOp::Stop
            }
            if token & (UPGRADABLE_BIT | WRITER_BIT) != 0 && state & UPGRADABLE_BIT != 0 {
                continue; // FilterOp::Skip
            }
            state += token;
            unparked[i] = true;
        }
        unparked
    }

    /// The waiters of `take_unpark_set` for a queue of `(min_permits, num_permits)` with
    /// `available` free permits: which ones it takes.
    fn shuttle_filter(queue: &[(usize, usize)], available: usize) -> Vec<bool> {
        let waiters = queue
            .iter()
            .map(|&(min_permits, num_permits)| {
                Arc::new(Waiter {
                    task_id: AtomicUsize::new(0),
                    num_permits,
                    min_permits,
                    is_queued: AtomicBool::new(true),
                    has_permits: AtomicBool::new(false),
                    woken: AtomicBool::new(false),
                    overtakes: AtomicUsize::new(0),
                    ticket: AtomicUsize::new(0),
                    park_unreported: AtomicBool::new(false),
                    clock: VectorClock::new(),
                    waker: Mutex::new(None),
                })
            })
            .collect::<Vec<_>>();
        let semaphore = BatchSemaphore::const_new(available, Fairness::ParkingLot);
        let mut state = semaphore.state.borrow_mut();
        state.waiters.extend(waiters.iter().cloned());
        let unparked = state.take_unpark_set();
        let taken = waiters
            .iter()
            .map(|waiter| unparked.iter().any(|w| Arc::ptr_eq(w, waiter)))
            .collect::<Vec<_>>();
        // The rest stay in the queue, in order.
        let stayed = waiters.iter().zip(&taken).filter(|(_, &taken)| !taken);
        assert!(stayed
            .map(|(waiter, _)| Arc::as_ptr(waiter))
            .eq(state.waiters.iter().map(Arc::as_ptr)));
        taken
    }

    fn permits(request: Request) -> (usize, usize) {
        match request {
            Request::Read => (1, 1),
            Request::UpgradableRead => (UPGRADABLE, UPGRADABLE),
            Request::Write => (WRITER_RESERVES_AT, N),
        }
    }

    /// Every queue of up to `max_len` requests.
    fn queues(max_len: usize) -> Vec<Vec<Request>> {
        let mut all = vec![vec![]];
        let mut longest = vec![vec![]];
        for _ in 0..max_len {
            longest = longest
                .iter()
                .flat_map(|queue: &Vec<Request>| {
                    [Request::Read, Request::UpgradableRead, Request::Write].map(|request| {
                        let mut queue = queue.clone();
                        queue.push(request);
                        queue
                    })
                })
                .collect();
            all.extend(longest.iter().cloned());
        }
        all
    }

    #[test]
    fn rwlock_wake_sets_match_parking_lot() {
        // Each release that unparks threads: `parking_lot`'s initial filter state, and the permits
        // that are free after it in Shuttle, while `readers` readers keep holding the lock.
        let mut releases = vec![
            ("unlock_exclusive", 0, 0, N),
            ("downgrade", 0, ONE_READER, N - 1),
            (
                "downgrade_to_upgradable",
                0,
                ONE_READER | UPGRADABLE_BIT,
                N - UPGRADABLE,
            ),
        ];
        for readers in 0..4 {
            releases.extend([
                ("unlock_upgradable", readers, 0, N - readers),
                ("downgrade_upgradable", readers, ONE_READER, N - 1 - readers),
                ("a writer that waits for the readers times out", readers, 0, N - readers),
                (
                    "an upgrade that waits for the readers times out",
                    readers,
                    ONE_READER | UPGRADABLE_BIT,
                    N - UPGRADABLE - readers,
                ),
            ]);
        }
        let mut checked = 0;
        for queue in queues(8) {
            let shape = queue.iter().map(|&request| permits(request)).collect::<Vec<_>>();
            for &(release, readers, state, available) in &releases {
                assert_eq!(
                    shuttle_filter(&shape, available),
                    parking_lot_filter(&queue, state),
                    "{release} with {readers} readers staying, queue {queue:?}"
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 186_979);
    }

    #[test]
    fn mutex_wakes_the_head_only() {
        for len in 0..8 {
            let unparked = shuttle_filter(&vec![(1, 1); len], 1);
            assert_eq!(unparked, (0..len).map(|i| i == 0).collect::<Vec<_>>());
        }
    }
}
