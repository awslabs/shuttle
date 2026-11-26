// This file is 99% copy-paste from tokio and Loom.
// This means that it should be a true model, but it uses `shuttle_tokio::sync::Semaphore`s instead of `tokio::sync::Semaphore`s.
use super::{Semaphore, SemaphorePermit, TryAcquireError};
use shuttle_engine::runtime::per_execution::PerExecution;
use shuttle_engine::runtime::thread;
use std::cell::Cell;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::mem::MaybeUninit;
use std::ops::Drop;
use std::ptr;

// Loom's `UnsafeCell` verbatim.
// Not `pub` in Tokio, so reimplemented here.
#[derive(Debug)]
pub(crate) struct UnsafeCell<T>(std::cell::UnsafeCell<T>);

impl<T> UnsafeCell<T> {
    pub(crate) const fn new(data: T) -> UnsafeCell<T> {
        UnsafeCell(std::cell::UnsafeCell::new(data))
    }

    #[inline(always)]
    pub(crate) fn with<R>(&self, f: impl FnOnce(*const T) -> R) -> R {
        f(self.0.get())
    }

    #[inline(always)]
    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        f(self.0.get())
    }
}

// This file contains an implementation of an OnceCell. The principle
// behind the safety the of the cell is that any thread with an `&OnceCell` may
// access the `value` field according the following rules:
//
//  1. When `value_set` is false, the `value` field may be modified by the
//     thread holding the permit on the semaphore.
//  2. When `value_set` is true, the `value` field may be accessed immutably by
//     any thread.
//
// It is an invariant that if the semaphore is closed, then `value_set` is true.
// The reverse does not necessarily hold — but if not, the semaphore may not
// have any available permits.
//
// A thread with a `&mut OnceCell` may modify the value in any way it wants as
// long as the invariants are upheld.

/// A cell's value in one execution (see [`PerExecution`]).
struct CellSlot<T> {
    /// Whether `value` holds a value. A `Cell` rather than an atomic, because a slot belongs to one
    /// execution, which runs on one thread.
    value_set: Cell<bool>,
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> CellSlot<T> {
    /// The slot of an execution that has not set a value yet.
    const fn empty() -> Self {
        Self {
            value_set: Cell::new(false),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// The slot of a cell that was created with a value, which every execution shares: such a cell
    /// is initialized from the start and never changes.
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

/// A thread-safe cell that can be written to only once.
///
/// # Differences from `tokio`
///
/// A cell that was created empty is empty again at the start of every execution, so that a cell in
/// a `static` does not pass the value one execution set on to the next, which would make the
/// executions depend on each other and a failing schedule hard to replay. The value lives in the
/// current execution's storage, which brings the rest of the differences:
///
/// * the methods that reach the value require `T: 'static`, which that storage requires;
/// * they have to run inside a Shuttle execution, including those of [`Clone`], [`PartialEq`] and
///   [`fmt::Debug`], which read the value. The constructors run anywhere, as tokio's do;
/// * a value is dropped when the execution ends, not when the cell is dropped, so a cell that goes
///   out of scope part way through an execution keeps its value until the end of it.
///
/// A cell created with a value ([`OnceCell::const_new_with`], [`OnceCell::new_with`] of a `Some`,
/// and `From<T>`) holds that one value in every execution: it is initialized from the start, so no
/// execution can change it, and nothing carries over.
pub struct OnceCell<T> {
    slot: PerExecution<CellSlot<T>>,
    semaphore: Semaphore,
}

impl<T> Default for OnceCell<T> {
    fn default() -> OnceCell<T> {
        OnceCell::new()
    }
}

impl<T: fmt::Debug + 'static> fmt::Debug for OnceCell<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("OnceCell").field("value", &self.get()).finish()
    }
}

impl<T: Clone + 'static> Clone for OnceCell<T> {
    fn clone(&self) -> OnceCell<T> {
        OnceCell::new_with(self.get().cloned())
    }
}

impl<T: PartialEq + 'static> PartialEq for OnceCell<T> {
    fn eq(&self, other: &OnceCell<T>) -> bool {
        self.get() == other.get()
    }
}

impl<T: Eq + 'static> Eq for OnceCell<T> {}

impl<T> From<T> for OnceCell<T> {
    fn from(value: T) -> Self {
        OnceCell {
            slot: PerExecution::shared(CellSlot::with_value(value)),
            semaphore: Semaphore::const_new(0), // `new_closed` in tokio
        }
    }
}

impl<T> OnceCell<T> {
    /// Creates a new empty `OnceCell` instance.
    pub fn new() -> Self {
        OnceCell {
            slot: PerExecution::new(),
            semaphore: Semaphore::new(1),
        }
    }

    /// Creates a new `OnceCell` that contains the provided value, if any.
    ///
    /// If the `Option` is `None`, this is equivalent to `OnceCell::new`.
    ///
    /// [`OnceCell::new`]: crate::sync::OnceCell::new
    // Once https://github.com/rust-lang/rust/issues/73255 lands
    // and tokio MSRV is bumped to the rustc version with it stablised,
    // we can made this function available in const context,
    // by creating `Semaphore::const_new_closed`.
    pub fn new_with(value: Option<T>) -> Self {
        if let Some(v) = value {
            OnceCell::from(v)
        } else {
            OnceCell::new()
        }
    }

    pub const fn const_new_with(value: T) -> Self {
        OnceCell {
            slot: PerExecution::shared(CellSlot::with_value(value)),
            semaphore: Semaphore::const_new(0), // `const_new_closed` in tokio
        }
    }

    pub const fn const_new() -> Self {
        OnceCell {
            slot: PerExecution::new(),
            semaphore: Semaphore::const_new(1),
        }
    }
}

impl<T: 'static> OnceCell<T> {
    /// The cell's slot in the current execution, which the execution creates, empty, when it first
    /// uses a cell that was created empty.
    fn slot(&self) -> &CellSlot<T> {
        self.slot.get(CellSlot::empty)
    }

    /// Returns `true` if the `OnceCell` currently contains a value, and `false`
    /// otherwise.
    pub fn initialized(&self) -> bool {
        self.slot().value_set.get()
    }

    // SAFETY: The OnceCell must not be empty.
    unsafe fn get_unchecked(&self) -> &T {
        &*self.slot().value.with(|ptr| (*ptr).as_ptr())
    }

    // SAFETY: The OnceCell must not be empty.
    unsafe fn get_unchecked_mut(&mut self) -> &mut T {
        &mut *self.slot().value.with_mut(|ptr| (*ptr).as_mut_ptr())
    }

    fn set_value(&self, value: T, permit: SemaphorePermit<'_>) -> &T {
        let slot = self.slot();
        // SAFETY: We are holding the only permit on the semaphore, and the slot is empty, so
        // nothing refers to its value.
        unsafe {
            slot.value.with_mut(|ptr| (*ptr).as_mut_ptr().write(value));
        }

        slot.value_set.set(true);
        self.semaphore.close();
        permit.forget();

        // SAFETY: We just initialized the cell.
        unsafe { self.get_unchecked() }
    }

    /// Returns a reference to the value currently stored in the `OnceCell`, or
    /// `None` if the `OnceCell` is empty.
    pub fn get(&self) -> Option<&T> {
        if self.initialized() {
            Some(unsafe { self.get_unchecked() })
        } else {
            None
        }
    }

    /// Returns a mutable reference to the value currently stored in the
    /// `OnceCell`, or `None` if the `OnceCell` is empty.
    ///
    /// Since this call borrows the `OnceCell` mutably, it is safe to mutate the
    /// value inside the `OnceCell` — the mutable borrow statically guarantees
    /// no other references exist.
    pub fn get_mut(&mut self) -> Option<&mut T> {
        if self.initialized() {
            Some(unsafe { self.get_unchecked_mut() })
        } else {
            None
        }
    }

    /// Sets the value of the `OnceCell` to the given value if the `OnceCell` is
    /// empty.
    ///
    /// If the `OnceCell` already has a value, this call will fail with an
    /// [`SetError::AlreadyInitializedError`].
    ///
    /// If the `OnceCell` is empty, but some other task is currently trying to
    /// set the value, this call will fail with [`SetError::InitializingError`].
    ///
    /// [`SetError::AlreadyInitializedError`]: crate::sync::SetError::AlreadyInitializedError
    /// [`SetError::InitializingError`]: crate::sync::SetError::InitializingError
    pub fn set(&self, value: T) -> Result<(), SetError<T>> {
        if self.initialized() {
            return Err(SetError::AlreadyInitializedError(value));
        }

        // Another task might be initializing the cell, in which case
        // `try_acquire` will return an error. If we succeed to acquire the
        // permit, then we can set the value.
        match self.semaphore.try_acquire() {
            Ok(permit) => {
                debug_assert!(!self.initialized());
                self.set_value(value, permit);
                Ok(())
            }
            Err(TryAcquireError::NoPermits) => {
                // Some other task is holding the permit. That task is
                // currently trying to initialize the value.
                Err(SetError::InitializingError(value))
            }
            Err(TryAcquireError::Closed) => {
                // The semaphore was closed. Some other task has initialized
                // the value.
                Err(SetError::AlreadyInitializedError(value))
            }
        }
    }

    /// Gets the value currently in the `OnceCell`, or initialize it with the
    /// given asynchronous operation.
    ///
    /// If some other task is currently working on initializing the `OnceCell`,
    /// this call will wait for that other task to finish, then return the value
    /// that the other task produced.
    ///
    /// If the provided operation is cancelled or panics, the initialization
    /// attempt is cancelled. If there are other tasks waiting for the value to
    /// be initialized, one of them will start another attempt at initializing
    /// the value.
    ///
    /// This will deadlock if `f` tries to initialize the cell recursively.
    pub async fn get_or_init<F, Fut>(&self, f: F) -> &T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        if self.initialized() {
            // SAFETY: The OnceCell has been fully initialized.
            unsafe { self.get_unchecked() }
        } else {
            // Here we try to acquire the semaphore permit. Holding the permit
            // will allow us to set the value of the OnceCell, and prevents
            // other tasks from initializing the OnceCell while we are holding
            // it.
            match self.semaphore.acquire().await {
                Ok(permit) => {
                    debug_assert!(!self.initialized());

                    // If `f()` panics or `select!` is called, this
                    // `get_or_init` call is aborted and the semaphore permit is
                    // dropped.
                    let value = f().await;

                    // Scheduling point precedes the visible store of the value
                    thread::switch();
                    self.set_value(value, permit)
                }
                Err(_) => {
                    debug_assert!(self.initialized());

                    // SAFETY: The semaphore has been closed. This only happens
                    // when the OnceCell is fully initialized.
                    unsafe { self.get_unchecked() }
                }
            }
        }
    }

    /// Gets the value currently in the `OnceCell`, or initialize it with the
    /// given asynchronous operation.
    ///
    /// If some other task is currently working on initializing the `OnceCell`,
    /// this call will wait for that other task to finish, then return the value
    /// that the other task produced.
    ///
    /// If the provided operation returns an error, is cancelled or panics, the
    /// initialization attempt is cancelled. If there are other tasks waiting
    /// for the value to be initialized, one of them will start another attempt
    /// at initializing the value.
    ///
    /// This will deadlock if `f` tries to initialize the cell recursively.
    pub async fn get_or_try_init<E, F, Fut>(&self, f: F) -> Result<&T, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        if self.initialized() {
            // SAFETY: The OnceCell has been fully initialized.
            unsafe { Ok(self.get_unchecked()) }
        } else {
            // Here we try to acquire the semaphore permit. Holding the permit
            // will allow us to set the value of the OnceCell, and prevents
            // other tasks from initializing the OnceCell while we are holding
            // it.
            match self.semaphore.acquire().await {
                Ok(permit) => {
                    debug_assert!(!self.initialized());

                    // If `f()` panics or `select!` is called, this
                    // `get_or_try_init` call is aborted and the semaphore
                    // permit is dropped.
                    let value = f().await;

                    match value {
                        Ok(value) => {
                            // Scheduling point precedes the visible store of the value
                            thread::switch();
                            Ok(self.set_value(value, permit))
                        }
                        Err(e) => Err(e),
                    }
                }
                Err(_) => {
                    debug_assert!(self.initialized());

                    // SAFETY: The semaphore has been closed. This only happens
                    // when the OnceCell is fully initialized.
                    unsafe { Ok(self.get_unchecked()) }
                }
            }
        }
    }

    /// Takes the value from the cell, destroying the cell in the process.
    /// Returns `None` if the cell is empty.
    pub fn into_inner(self) -> Option<T> {
        let slot = self.slot();
        if slot.value_set.get() {
            // Set to uninitialized so that the slot's destructor does not drop the value too
            slot.value_set.set(false);
            // SAFETY: the slot held a value, and `&mut self` rules out any reference to it. A slot
            // that every execution shares is only reached through `&self`, so it cannot get here.
            Some(unsafe { slot.value.with(|ptr| ptr::read(ptr).assume_init()) })
        } else {
            None
        }
    }

    /// Takes ownership of the current value, leaving the cell empty.  Returns
    /// `None` if the cell is empty.
    pub fn take(&mut self) -> Option<T> {
        std::mem::take(self).into_inner()
    }
}

// Since `get` gives us access to immutable references of the OnceCell, OnceCell
// can only be Sync if T is Sync, otherwise OnceCell would allow sharing
// references of !Sync values across threads. We need T to be Send in order for
// OnceCell to by Sync because we can use `set` on `&OnceCell<T>` to send values
// (of type T) across threads.
unsafe impl<T: Sync + Send> Sync for OnceCell<T> {}

// Access to OnceCell's value is guarded by the semaphore permit
// and atomic operations on `value_set`, so as long as T itself is Send
// it's safe to send it to another thread
unsafe impl<T: Send> Send for OnceCell<T> {}

/// Errors that can be returned from [`OnceCell::set`].
///
/// [`OnceCell::set`]: crate::sync::OnceCell::set
#[derive(Debug, PartialEq, Eq)]
pub enum SetError<T> {
    /// The cell was already initialized when [`OnceCell::set`] was called.
    ///
    /// [`OnceCell::set`]: crate::sync::OnceCell::set
    AlreadyInitializedError(T),

    /// The cell is currently being initialized.
    InitializingError(T),
}

impl<T> fmt::Display for SetError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetError::AlreadyInitializedError(_) => write!(f, "AlreadyInitializedError"),
            SetError::InitializingError(_) => write!(f, "InitializingError"),
        }
    }
}

impl<T: fmt::Debug> Error for SetError<T> {}

impl<T> SetError<T> {
    /// Whether `SetError` is `SetError::AlreadyInitializedError`.
    pub fn is_already_init_err(&self) -> bool {
        match self {
            SetError::AlreadyInitializedError(_) => true,
            SetError::InitializingError(_) => false,
        }
    }

    /// Whether `SetError` is `SetError::InitializingError`
    pub fn is_initializing_err(&self) -> bool {
        match self {
            SetError::AlreadyInitializedError(_) => false,
            SetError::InitializingError(_) => true,
        }
    }
}
