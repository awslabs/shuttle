//! A port of tokio's `sync/set_once.rs` (tokio 1.54). The API and the semantics are tokio's; what
//! changes is how the waiters of `wait` wait, and where the scheduling points are. The places that
//! differ from tokio are marked with `SHUTTLE_CHANGES`.
//!
//! tokio parks the waiters of `wait` on a `Notify`, which `set` notifies, and a waiter re-checks the
//! value after it registers, because a notification sent before it registered would be lost. Here
//! they queue on a `BatchSemaphore` that never has a permit, and that `set` closes once the value is
//! stored, as this crate's `OnceCell` closes its semaphore. A closed semaphore stays closed, so there
//! is no notification to lose: a waiter that queues before `set` is woken by the close, and one that
//! tries to queue after it fails to at once.
//!
//! Each operation that reads or writes the cell takes one scheduling point, before it does, and then
//! runs in one step. `set` is one step from its check of the cell to the close, which is what makes
//! concurrent `set`s exclusive here, and has the effect that the lock tokio holds there has: a
//! reader sees either no value or the whole of it, and of two `set`s exactly one succeeds.
//!
//! The value lives in the current execution's state rather than in the cell, so that a `SetOnce` in
//! a `static` is empty again at the start of every execution (see [`SetOnce`]).

use super::once_cell::UnsafeCell;
use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use shuttle_engine::runtime::per_execution::PerExecution;
use shuttle_engine::runtime::thread;
use std::cell::Cell;
use std::error::Error;
use std::fmt;
use std::mem::MaybeUninit;
use std::ops::Drop;
use std::ptr;

// This file contains an implementation of an SetOnce. The value of SetOnce
// can only be modified once during initialization.
//
//  1. When `value_set` is false, the `value` is not initialized and wait()
//      future will keep on waiting.
//  2. When `value_set` is true, the wait() future completes, get() will return
//      Some(&T)
//
// The value cannot be changed after set() is called. Subsequent calls to set()
// will return a `SetOnceError`.
//
// SHUTTLE_CHANGES: It is an invariant that if `waiters` is closed, then `value_set` is true. The
// reverse does not hold for a `SetOnce` made with a value, whose semaphore cannot be closed in a
// `const fn`; but `wait` only queues on the semaphore after it has found the cell empty, which such
// a `SetOnce` never is.

/// A thread-safe cell that can be written to only once.
///
/// A `SetOnce` is inspired from python's [`asyncio.Event`] type. It can be
/// used to wait until the value of the `SetOnce` is set like a "Event" mechanism.
///
/// A `SetOnce` is typically used for global variables that need to be
/// initialized once on first use, but need no further changes. The `SetOnce`
/// in Tokio allows the initialization procedure to be asynchronous.
///
/// # Example
///
/// ```
/// # async fn dox() {
/// use shuttle_tokio_impl_inner::sync::SetOnce;
/// use std::sync::Arc;
///
/// let once = SetOnce::new();
///
/// let arc = Arc::new(once);
/// let first_cl = Arc::clone(&arc);
/// let second_cl = Arc::clone(&arc);
///
/// // set the value inside a task
/// shuttle::future::spawn(async move { first_cl.set(20) }).await.unwrap().unwrap();
///
/// // wait inside task to not block the main thread
/// shuttle::future::spawn(async move {
///     // wait inside async context for the value to be set
///     assert_eq!(*second_cl.wait().await, 20);
/// }).await.unwrap();
///
/// // subsequent set calls will fail
/// assert!(arc.set(30).is_err());
/// # }
/// ```
///
/// # Differences from `tokio`
///
/// SHUTTLE_CHANGES: a `SetOnce` that was created empty is empty again at the start of every
/// execution, so that one in a `static` does not pass the value an execution set on to the next,
/// which would make the executions depend on each other and a failing schedule hard to replay. The
/// value lives in the current execution's storage, which brings the rest of the differences:
///
/// * the methods that reach the value require `T: 'static`, which that storage requires;
/// * they have to run inside a Shuttle execution, including those of [`Clone`], [`PartialEq`] and
///   [`fmt::Debug`], which read the value. The constructors run anywhere, as tokio's do;
/// * a value is dropped when the execution ends, not when the cell is dropped, so a cell that goes
///   out of scope part way through an execution keeps its value until the end of it.
///
/// A `SetOnce` created with a value ([`SetOnce::const_new_with`], [`SetOnce::new_with`] of a `Some`,
/// and `From<T>`) holds that one value in every execution: it is set from the start, so no
/// execution can change it, and nothing carries over.
///
/// [`asyncio.Event`]: https://docs.python.org/3/library/asyncio-sync.html#asyncio.Event
pub struct SetOnce<T> {
    // SHUTTLE_CHANGES: the value lives in a `PerExecution`, so that a `SetOnce` created empty is
    // empty again at the start of every execution (see the type's docs).
    slot: PerExecution<CellSlot<T>>,
    // SHUTTLE_CHANGES: tokio has a `Notify` here. The waiters of `wait` queue on this semaphore,
    // which never has a permit, and which `set` closes (see the module docs).
    waiters: BatchSemaphore,
}

/// A cell's value in one execution (see [`PerExecution`]).
struct CellSlot<T> {
    /// Whether `value` holds a value. A `Cell` rather than an atomic, because a slot belongs to one
    /// execution, which runs on one thread.
    value_set: Cell<bool>,
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> CellSlot<T> {
    /// The slot of an execution in which nothing has set the value yet.
    const fn empty() -> Self {
        Self {
            value_set: Cell::new(false),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// The slot of a `SetOnce` that was created with a value, which every execution shares: such a
    /// cell is set from the start and never changes.
    const fn with_value(value: T) -> Self {
        Self {
            value_set: Cell::new(true),
            value: UnsafeCell::new(MaybeUninit::new(value)),
        }
    }
}

impl<T> Drop for CellSlot<T> {
    fn drop(&mut self) {
        if self.value_set.get() {
            // SAFETY: the slot holds a value, and this is the last use of the slot.
            unsafe { self.value.with_mut(|ptr| ptr::drop_in_place((*ptr).as_mut_ptr())) };
        }
    }
}

impl<T> Default for SetOnce<T> {
    fn default() -> SetOnce<T> {
        SetOnce::new()
    }
}

impl<T: fmt::Debug + 'static> fmt::Debug for SetOnce<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        // SHUTTLE_CHANGES: reads the value without the scheduling point of `get`, so that whether a
        // `SetOnce` is formatted, by a `tracing` statement say, does not change the schedule.
        fmt.debug_struct("SetOnce")
            .field("value", &self.get_no_scheduling_point())
            .finish()
    }
}

impl<T: Clone + 'static> Clone for SetOnce<T> {
    fn clone(&self) -> SetOnce<T> {
        SetOnce::new_with(self.get().cloned())
    }
}

impl<T: PartialEq + 'static> PartialEq for SetOnce<T> {
    fn eq(&self, other: &SetOnce<T>) -> bool {
        self.get() == other.get()
    }
}

impl<T: Eq + 'static> Eq for SetOnce<T> {}

impl<T> From<T> for SetOnce<T> {
    fn from(value: T) -> Self {
        SetOnce {
            slot: PerExecution::shared(CellSlot::with_value(value)),
            waiters: BatchSemaphore::new(0, Fairness::StrictlyFair),
        }
    }
}

impl<T> SetOnce<T> {
    /// Creates a new empty `SetOnce` instance.
    pub fn new() -> Self {
        Self {
            slot: PerExecution::new(),
            waiters: BatchSemaphore::new(0, Fairness::StrictlyFair),
        }
    }

    /// Creates a new empty `SetOnce` instance.
    ///
    /// Equivalent to `SetOnce::new`, except that it can be used in static
    /// variables.
    ///
    pub const fn const_new() -> Self {
        Self {
            slot: PerExecution::new(),
            waiters: BatchSemaphore::const_new(0, Fairness::StrictlyFair),
        }
    }

    /// Creates a new `SetOnce` that contains the provided value, if any.
    ///
    /// If the `Option` is `None`, this is equivalent to `SetOnce::new`.
    ///
    /// [`SetOnce::new`]: crate::sync::SetOnce::new
    pub fn new_with(value: Option<T>) -> Self {
        if let Some(v) = value {
            SetOnce::from(v)
        } else {
            SetOnce::new()
        }
    }

    /// Creates a new `SetOnce` that contains the provided value.
    ///
    /// Equivalent to `SetOnce::new_with(Some(value))`, except that it can be used in static
    /// variables.
    pub const fn const_new_with(value: T) -> Self {
        Self {
            slot: PerExecution::shared(CellSlot::with_value(value)),
            waiters: BatchSemaphore::const_new(0, Fairness::StrictlyFair),
        }
    }
}

impl<T: 'static> SetOnce<T> {
    /// The cell's slot in the current execution, which the execution creates, empty, when it first
    /// uses a `SetOnce` that was created empty.
    fn slot(&self) -> &CellSlot<T> {
        self.slot.get(CellSlot::empty)
    }

    /// Returns `true` if the `SetOnce` currently contains a value, and `false`
    /// otherwise.
    pub fn initialized(&self) -> bool {
        // SHUTTLE_CHANGES: a scheduling point precedes the read, so that Shuttle runs a `set` in
        // another task both before and after it. It is skipped where there is nothing to switch
        // to, as outside of a Shuttle execution, since a `SetOnce` in a `static` may well be read
        // there.
        thread::switch_if_not_stopping();
        self.initialized_no_scheduling_point()
    }

    fn initialized_no_scheduling_point(&self) -> bool {
        self.slot().value_set.get()
    }

    // SAFETY: The SetOnce must not be empty.
    unsafe fn get_unchecked(&self) -> &T {
        &*self.slot().value.with(|ptr| (*ptr).as_ptr())
    }

    /// Returns a reference to the value currently stored in the `SetOnce`, or
    /// `None` if the `SetOnce` is empty.
    pub fn get(&self) -> Option<&T> {
        if self.initialized() {
            // SAFETY: the SetOnce is initialized, so we can safely
            // call get_unchecked and return the value
            Some(unsafe { self.get_unchecked() })
        } else {
            None
        }
    }

    fn get_no_scheduling_point(&self) -> Option<&T> {
        if self.initialized_no_scheduling_point() {
            // SAFETY: as in `get`.
            Some(unsafe { self.get_unchecked() })
        } else {
            None
        }
    }

    /// Sets the value of the `SetOnce` to the given value if the `SetOnce` is
    /// empty.
    ///
    /// If the `SetOnce` already has a value, this call will fail with an
    /// [`SetOnceError`].
    ///
    /// [`SetOnceError`]: crate::sync::SetOnceError
    pub fn set(&self, value: T) -> Result<(), SetOnceError<T>> {
        // SHUTTLE_CHANGES: a scheduling point precedes the check of the cell, and the rest is one
        // step. tokio checks the cell, takes the lock of its `Notify`'s waiter list, and checks
        // again, and holds the lock until the waiters are notified; one step has the same effect
        // (see the module docs). As in `initialized`, it is skipped where there is nothing to switch
        // to.
        thread::switch_if_not_stopping();

        let slot = self.slot();
        if slot.value_set.get() {
            return Err(SetOnceError(value));
        }

        // SAFETY: No other `set` can run until this one returns, as there is no scheduling point
        // before then, and nothing reads the value until `value_set` is true.
        unsafe {
            slot.value.with_mut(|ptr| (*ptr).as_mut_ptr().write(value));
        }

        slot.value_set.set(true);

        // notify the waiting wakers that the value is set
        //
        // SHUTTLE_CHANGES: wakes the waiters of `wait` by closing their semaphore, without the
        // scheduling point of `close`: this is still the step that stored the value.
        self.waiters.close_no_scheduling_point();

        Ok(())
    }

    /// Takes the value from the cell, destroying the cell in the process.
    /// Returns `None` if the cell is empty.
    pub fn into_inner(self) -> Option<T> {
        let slot = self.slot();
        if slot.value_set.get() {
            // Since we have taken ownership of self, the slot's drop implementation will be called
            // by the end of this function, so to prevent a double free we set `value_set` to false
            // so that it does not try to drop the value again.
            slot.value_set.set(false);

            // SAFETY: The SetOnce is currently initialized, we can assume the
            // value is initialized and return that, when we return the value
            // we give the drop handler to the return scope.
            Some(unsafe { slot.value.with_mut(|ptr| ptr::read(ptr).assume_init()) })
        } else {
            None
        }
    }

    /// Waits until the value is set.
    ///
    /// If the `SetOnce` is already initialized, it will return the value
    /// immediately.
    ///
    /// # Cancel safety
    ///
    /// This method is cancel safe.
    pub async fn wait(&self) -> &T {
        // SHUTTLE_CHANGES: tokio loops, registering with its `Notify` and checking the value again
        // each time. A closed semaphore stays closed, so one check and one acquire are enough (see
        // the module docs). Both take a scheduling point first.
        if let Some(value) = self.get() {
            return value;
        }

        // The semaphore has no permits and is never given any, so the acquire only returns once
        // `set` has closed it, and then with an error. A `wait` that is dropped while it is queued
        // leaves the queue, so this is cancel safe, as tokio's is.
        match self.waiters.acquire(1).await {
            // SAFETY: the semaphore is only closed once the value is stored.
            Err(_) => unsafe { self.get_unchecked() },
            Ok(()) => unreachable!("the semaphore of a `SetOnce` has no permits to acquire"),
        }
    }
}

// Since `get` gives us access to immutable references of the SetOnce, SetOnce
// can only be Sync if T is Sync, otherwise SetOnce would allow sharing
// references of !Sync values across threads. We need T to be Send in order for
// SetOnce to by Sync because we can use `set` on `&SetOnce<T>` to send values
// (of type T) across threads.
unsafe impl<T: Sync + Send> Sync for SetOnce<T> {}

// Access to SetOnce's value is guarded by the Atomic boolean flag
// and atomic operations on `value_set`, so as long as T itself is Send
// it's safe to send it to another thread
unsafe impl<T: Send> Send for SetOnce<T> {}

/// Error that can be returned from [`SetOnce::set`].
///
/// This error means that the `SetOnce` was already initialized when
/// set was called
///
/// [`SetOnce::set`]: crate::sync::SetOnce::set
#[derive(Debug, PartialEq, Eq)]
pub struct SetOnceError<T>(pub T);

impl<T> fmt::Display for SetOnceError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SetOnceError")
    }
}

impl<T: fmt::Debug> Error for SetOnceError<T> {}
