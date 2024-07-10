//! A counting semaphore supporting both async and sync operations.
use crate::runtime::execution::ExecutionState;
use crate::runtime::task::TaskId;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};
use tracing::trace;

#[derive(Debug)]
struct Waiter {
    task_id: TaskId,
    num_permits: usize,
    is_queued: AtomicBool,
    has_permits: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Waiter {
    fn new(num_permits: usize) -> Self {
        Self {
            task_id: ExecutionState::me(),
            num_permits,
            is_queued: AtomicBool::new(false),
            has_permits: AtomicBool::new(false),
            waker: Mutex::new(None),
        }
    }
}

/// A counting semaphore which permits waiting on multiple permits at once,
/// and supports both asychronous and synchronous blocking operations.
///
/// The semaphore is strictly fair, so earlier requesters always get priority
/// over later ones.
///
/// TODO: Provide an option to support weaker models for fairness.
#[derive(Debug)]
struct BatchSemaphoreState {
    // Key invariants:
    //
    // (1) if `waiters` is nonempty and the head waiter is `H`,
    // then `H.num_permits > permits_available`.  (In other words, we are
    // never in a state where there are enough permits available for the
    // first waiter.  This invariant is ensured by the `drop` handler below.)
    //
    // (2) W is in waiters iff W.is_queued
    //
    // (3) W.is_queued ==> !W.has_permits
    // Note: the converse is not true.  We can have !W.has_permits && !W.is_queued
    // when the Acquire is created but not yet polled.
    //
    // (4) closed ==> waiters.is_empty()
    waiters: VecDeque<Arc<Waiter>>,
    permits_available: usize,
    closed: bool,
}

impl BatchSemaphoreState {
    fn acquire_permits(&mut self, num_permits: usize) -> Result<(), TryAcquireError> {
        if self.closed {
            Err(TryAcquireError::Closed)
        } else if self.waiters.is_empty() && num_permits <= self.permits_available {
            // No one is waiting and there are enough permits available
            self.permits_available -= num_permits;
            Ok(())
        } else {
            Err(TryAcquireError::NoPermits)
        }
    }
}

/// Counting semaphore
#[derive(Debug)]
pub struct BatchSemaphore {
    state: RefCell<BatchSemaphoreState>,
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
    pub fn new(num_permits: usize) -> Self {
        let state = RefCell::new(BatchSemaphoreState {
            waiters: VecDeque::new(),
            permits_available: num_permits,
            closed: false,
        });
        Self { state }
    }

    /// Creates a new semaphore with the initial number of permits.
    pub const fn const_new(num_permits: usize) -> Self {
        let state = RefCell::new(BatchSemaphoreState {
            waiters: VecDeque::new(),
            permits_available: num_permits,
            closed: false,
        });
        Self { state }
    }

    /// Returns the current number of available permits.
    pub fn available_permits(&self) -> usize {
        let state = self.state.borrow();
        state.permits_available
    }

    /// Closes the semaphore. This prevents the semaphore from issuing new
    /// permits and notifies all pending waiters.
    pub fn close(&self) {
        let mut state = self.state.borrow_mut();
        state.closed = true;
        // Wake up all the waiters.  Since we've marked the state as closed, they
        // will all return `AcquireError::closed` from their acquire calls.
        let ptr = &*state as *const BatchSemaphoreState;
        for waiter in state.waiters.drain(..) {
            trace!(
                "semaphore {:p} removing and waking up waiter {:?} on close",
                ptr,
                waiter,
            );
            assert!(waiter.is_queued.swap(false, Ordering::SeqCst));
            assert!(!waiter.has_permits.load(Ordering::SeqCst)); // sanity check
            ExecutionState::with(|exec_state| {
                if !exec_state.in_cleanup() {
                    exec_state.get_mut(waiter.task_id).unblock();
                }
            });
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
        let mut state = self.state.borrow_mut();
        state.acquire_permits(num_permits)
    }

    fn enqueue_waiter(&self, waiter: &Arc<Waiter>) {
        let mut state = self.state.borrow_mut();

        trace!("enqueuing waiter {:?} for semaphore {:p}", waiter, &self.state);
        state.waiters.push_back(waiter.clone());

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
    }

    /// Acquire the specified number of permits (async API)
    pub fn acquire(&self, num_permits: usize) -> Acquire<'_> {
        Acquire::new(self, num_permits)
    }

    /// Acquire the specified number of permits (blocking API)
    pub fn acquire_blocking(&self, num_permits: usize) -> Result<(), AcquireError> {
        crate::future::block_on(self.acquire(num_permits))
    }

    /// Release `num_permits` back to the Semaphore
    pub fn release(&self, num_permits: usize) {
        if num_permits == 0 {
            return;
        }

        if ExecutionState::should_stop() {
            return;
        }

        let mut state = self.state.borrow_mut();

        // Permits released into the semaphore reflect the releasing thread's
        // clock; future acquires of those permits are causally dependent on
        // this event.
        ExecutionState::with(|s| {
            let clock = s.increment_clock();
            state.permits_available.release(num_permits, clock.clone());
        });

        let me = ExecutionState::me();
        trace!(task = ?me, avail = ?state.permits_available, waiters = ?state.waiters, "released {} permits for semaphore {:p}", num_permits, &self.state);

        while let Some(front) = state.waiters.front() {
            if front.num_permits <= state.permits_available {
                trace!("granted {:?} permits to waiter {:?}", front.num_permits, front);
                state.permits_available -= front.num_permits;
                let waiter = state.waiters.pop_front().unwrap();
                // Update waiter state as it is no longer in the queue
                assert!(waiter.is_queued.swap(false, Ordering::SeqCst));
                assert!(!waiter.has_permits.swap(true, Ordering::SeqCst));
                ExecutionState::with(|s| {
                    let task = s.get_mut(waiter.task_id);
                    assert!(!task.finished());
                    task.unblock();
                });
                let mut maybe_waker = waiter.waker.lock().unwrap();
                if let Some(waker) = maybe_waker.take() {
                    waker.wake();
                }
            } else {
                break;
            }
        }
        drop(state);

        // Releasing a semaphore is a yield point
        crate::runtime::thread::switch();
    }
}

// Safety: Semaphore is never actually passed across true threads, only across continuations. The
// RefCell<_> type therefore can't be preempted mid-bookkeeping-operation.
// TODO we shouldn't need to do this, but RefCell is not Send, and anything we put within a Semaphore
// TODO needs to be Send.
unsafe impl Send for BatchSemaphore {}
unsafe impl Sync for BatchSemaphore {}

impl Default for BatchSemaphore {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

/// The future that results from async calls to `acquire*`.
/// Callers must `await` on this future to obtain the necessary permits.
#[derive(Debug)]
pub struct Acquire<'a> {
    waiter: Arc<Waiter>,
    semaphore: &'a BatchSemaphore,
    completed: bool, // Has the future completed yet?
}

impl<'a> Acquire<'a> {
    fn new(semaphore: &'a BatchSemaphore, num_permits: usize) -> Self {
        let waiter = Arc::new(Waiter::new(num_permits));
        Self {
            waiter,
            semaphore,
            completed: false,
        }
    }
}

impl Future for Acquire<'_> {
    type Output = Result<(), AcquireError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        assert!(!self.completed);
        if self.waiter.has_permits.load(Ordering::SeqCst) {
            assert!(!self.waiter.is_queued.load(Ordering::SeqCst));
            self.completed = true;
            trace!("Acquire::poll for waiter {:?} with permits", self.waiter);
            Poll::Ready(Ok(()))
        } else if self.semaphore.is_closed() {
            assert!(!self.waiter.is_queued.load(Ordering::SeqCst));
            self.completed = true;
            trace!("Acquire::poll for waiter {:?} with closed", self.waiter);
            Poll::Ready(Err(AcquireError::closed()))
        } else if self.waiter.is_queued.load(Ordering::SeqCst) {
            trace!("Acquire::poll for waiter {:?} already queued", self.waiter);
            assert!(self.waiter.waker.lock().unwrap().is_some());
            Poll::Pending
        } else {
            match self.semaphore.try_acquire(self.waiter.num_permits) {
                Ok(()) => {
                    assert!(!self.waiter.is_queued.load(Ordering::SeqCst));
                    self.waiter.has_permits.store(true, Ordering::SeqCst);
                    self.completed = true;
                    trace!("Acquire::poll for waiter {:?} that got permits", self.waiter);
                    crate::runtime::thread::switch();
                    Poll::Ready(Ok(()))
                }
                Err(TryAcquireError::NoPermits) => {
                    let mut maybe_waker = self.waiter.waker.lock().unwrap();
                    *maybe_waker = Some(cx.waker().clone());
                    self.semaphore.enqueue_waiter(&self.waiter);
                    trace!("Acquire::poll for waiter {:?} that is enqueued", self.waiter);
                    Poll::Pending
                }
                Err(TryAcquireError::Closed) => unreachable!(),
            }
        }
    }
}

impl Drop for Acquire<'_> {
    fn drop(&mut self) {
        trace!("Acquire::drop for Acquire {:p} with waiter {:?}", self, self.waiter);
        if self.waiter.is_queued.load(Ordering::SeqCst) {
            // If the associated waiter is in the wait list, remove it
            self.semaphore.remove_waiter(&self.waiter);
        } else if self.waiter.has_permits.load(Ordering::SeqCst) && !self.completed {
            // If the waiter was granted permits, release them
            self.semaphore.release(self.waiter.num_permits);
        }
    }
}
