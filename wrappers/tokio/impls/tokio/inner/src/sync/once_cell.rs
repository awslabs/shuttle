// This file is 99% copy-paste from tokio and Loom.
// This means that it should be a true model, but it uses `shuttle_tokio::sync::Semaphore`s instead of `tokio::sync::Semaphore`s.
use super::{Semaphore, SemaphorePermit, TryAcquireError};
use shuttle_engine::runtime::thread;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::mem::MaybeUninit;
use std::ops::Drop;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

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
// In Shuttle this invariant can break. `BatchSemaphore` also closes itself when
// a permit is released while `ExecutionState::should_stop()` is true (see
// `BatchSemaphore::release`), for example after the execution stopped or
// failed, or outside an execution. If the permit of an unfinished
// `get_or_init` is released that way, the semaphore is closed while
// `value_set` is false. The SAFETY comments in `get_or_init` and
// `get_or_try_init` say what then goes wrong.
//
// A thread with a `&mut OnceCell` may modify the value in any way it wants as
// long as the invariants are upheld.

/// A thread-safe cell that can be written to only once.
pub struct OnceCell<T> {
    value_set: AtomicBool,
    value: UnsafeCell<MaybeUninit<T>>,
    semaphore: Semaphore,
}

impl<T> Default for OnceCell<T> {
    fn default() -> OnceCell<T> {
        OnceCell::new()
    }
}

impl<T: fmt::Debug> fmt::Debug for OnceCell<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("OnceCell").field("value", &self.get()).finish()
    }
}

impl<T: Clone> Clone for OnceCell<T> {
    fn clone(&self) -> OnceCell<T> {
        OnceCell::new_with(self.get().cloned())
    }
}

impl<T: PartialEq> PartialEq for OnceCell<T> {
    fn eq(&self, other: &OnceCell<T>) -> bool {
        self.get() == other.get()
    }
}

impl<T: Eq> Eq for OnceCell<T> {}

impl<T> Drop for OnceCell<T> {
    fn drop(&mut self) {
        if self.initialized_mut() {
            // SAFETY:
            // In order for the call to be safe, we need to ensure:
            // * `value` holds an initialized `T`.
            // * nothing uses that `T` after it is dropped.
            //
            // `value_set` is true, and it is true only once `value` holds a `T` (see `set_value`,
            // `From<T>` and `const_new_with`). `into_inner` sets it to false before it moves the
            // `T` out.
            // The cell is being dropped, and `&mut self` means that no reference into it is live.
            unsafe {
                self.value.with_mut(|ptr| ptr::drop_in_place((*ptr).as_mut_ptr()));
            };
        }
    }
}

impl<T> From<T> for OnceCell<T> {
    fn from(value: T) -> Self {
        OnceCell {
            value_set: AtomicBool::new(true),
            value: UnsafeCell::new(MaybeUninit::new(value)),
            semaphore: Semaphore::new(0), // `new_closed` in tokio
        }
    }
}

impl<T> OnceCell<T> {
    /// Creates a new empty `OnceCell` instance.
    pub fn new() -> Self {
        OnceCell {
            value_set: AtomicBool::new(false),
            value: UnsafeCell::new(MaybeUninit::uninit()),
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
            value_set: AtomicBool::new(true),
            value: UnsafeCell::new(MaybeUninit::new(value)),
            semaphore: Semaphore::const_new(0), // `const_new_closed` in tokio
        }
    }

    pub const fn const_new() -> Self {
        OnceCell {
            value_set: AtomicBool::new(false),
            value: UnsafeCell::new(MaybeUninit::uninit()),
            semaphore: Semaphore::const_new(1),
        }
    }

    /// Returns `true` if the `OnceCell` currently contains a value, and `false`
    /// otherwise.
    pub fn initialized(&self) -> bool {
        // Using acquire ordering so any threads that read a true from this
        // atomic is able to read the value.
        self.value_set.load(Ordering::Acquire)
    }

    /// Returns `true` if the `OnceCell` currently contains a value, and `false`
    /// otherwise.
    fn initialized_mut(&mut self) -> bool {
        *self.value_set.get_mut()
    }

    /// # Safety
    ///
    /// The caller must ensure:
    /// * `value` holds an initialized `T`.
    /// * the write that stored it happens before this call. For example, the caller saw
    ///   `value_set` as true with an acquire load.
    ///
    /// The caller does not have to ensure that the value stays, or that no `&mut T` is made while
    /// the returned reference lives. The returned reference borrows `self`, and each method that
    /// removes the value or makes a `&mut T` takes `&mut self`. And `set_value` never writes a cell
    /// that holds a value, as only the holder of the semaphore's only permit calls it, before
    /// `value_set` is true.
    unsafe fn get_unchecked(&self) -> &T {
        // SAFETY:
        // In order for the two dereferences to be safe, we need to ensure:
        // * `ptr` points to a live, aligned `MaybeUninit<T>` that no `&mut` aliases.
        // * the `T` in it is initialized, and the write that stored it happens before this read.
        //
        // `with` passes the pointer of the `UnsafeCell` in `value`, which `self` keeps alive. Only
        // `get_unchecked_mut` makes a `&mut` into it, and it takes `&mut self`, which the borrow of
        // `self` here excludes. `set_value` only writes through a raw pointer.
        // The caller ensures the second, so `&*` makes a reference to an initialized `T`.
        unsafe { &*self.value.with(|ptr| (*ptr).as_ptr()) }
    }

    /// # Safety
    ///
    /// The caller must ensure:
    /// * `value` holds an initialized `T`.
    ///
    /// The returned reference borrows `self` mutably, so no other reference into the cell is live
    /// while it is.
    unsafe fn get_unchecked_mut(&mut self) -> &mut T {
        // SAFETY:
        // In order for the two dereferences to be safe, we need to ensure:
        // * `ptr` points to a live, aligned `MaybeUninit<T>`, and no other reference to it is live.
        // * the `T` in it is initialized.
        //
        // `with_mut` passes the pointer of the `UnsafeCell` in `value`, which `self` keeps alive.
        // `&mut self` means that no other reference into the cell is live.
        // The caller ensures the second, so `&mut *` makes a reference to an initialized `T`.
        unsafe { &mut *self.value.with_mut(|ptr| (*ptr).as_mut_ptr()) }
    }

    fn set_value(&self, value: T, permit: SemaphorePermit<'_>) -> &T {
        // SAFETY:
        // In order for the write to be safe, we need to ensure:
        // * no other task reads or writes `value` at the same time.
        // * `value` holds no `T` that the write would overwrite.
        //
        // The caller holds `permit`, the semaphore's only permit. `new` and `const_new` make the
        // semaphore with one permit, and the cells that `From<T>` and `const_new_with` make hold a
        // value from the start, so no caller gets a permit for them. Another task writes `value`
        // only in this function, so only with the permit. A task reads `value` only after it sees
        // `value_set` as true, and that is set only below, after the write. The `Err` arms of
        // `get_or_init` and `get_or_try_init` are the exception, see their SAFETY comments.
        // `value_set` is false. It becomes true only below, and then the semaphore is closed and
        // the permit forgotten, so no later caller gets a permit. While it is false, `value` holds
        // no `T`: `into_inner` sets it to false only when it moves the `T` out.
        unsafe {
            self.value.with_mut(|ptr| (*ptr).as_mut_ptr().write(value));
        }

        // Using release ordering so any threads that read a true from this
        // atomic is able to read the value we just stored.
        self.value_set.store(true, Ordering::Release);
        self.semaphore.close();
        permit.forget();

        // SAFETY:
        // In order for the call to be safe, we need to ensure:
        // * `value` holds an initialized `T`.
        // * the write that stored it happens before this call.
        //
        // The write above stored it, in this task.
        unsafe { self.get_unchecked() }
    }

    /// Returns a reference to the value currently stored in the `OnceCell`, or
    /// `None` if the `OnceCell` is empty.
    pub fn get(&self) -> Option<&T> {
        if self.initialized() {
            // SAFETY:
            // In order for the call to be safe, we need to ensure:
            // * `value` holds an initialized `T`.
            // * the write that stored it happens before this call.
            //
            // `initialized` read `value_set` as true. `set_value` sets it with release ordering,
            // after it writes the value, and `initialized` reads it with acquire ordering. So the
            // value is there, and the write happens before this call.
            let value = unsafe { self.get_unchecked() };
            Some(value)
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
        if self.initialized_mut() {
            // SAFETY:
            // In order for the call to be safe, we need to ensure:
            // * `value` holds an initialized `T`.
            //
            // `value_set` is true, which it is only once `value` holds a `T` (see `Drop`).
            let value = unsafe { self.get_unchecked_mut() };
            Some(value)
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
            // SAFETY:
            // In order for the call to be safe, we need to ensure:
            // * `value` holds an initialized `T`.
            // * the write that stored it happens before this call.
            //
            // `initialized` read `value_set` as true with acquire ordering, so both hold (see
            // `get`).
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

                    // SAFETY:
                    // In order for the call to be safe, we need to ensure:
                    // * `value` holds an initialized `T`.
                    // * the write that stored it happens before this call.
                    //
                    // `acquire` fails only if the semaphore is closed. In tokio, only `set_value`
                    // closes it, after it stores the value, so the value is there, and the store
                    // happens before the close that this `acquire` saw. In Shuttle that does not
                    // always hold. `BatchSemaphore` also closes itself when a permit is released
                    // while `ExecutionState::should_stop()` is true (see
                    // `BatchSemaphore::release`), for example after the execution stopped or
                    // failed, or outside an execution. If that permit was the one of an unfinished
                    // `get_or_init` or `get_or_try_init`, the cell is still empty, and this reads
                    // a `T` that was never written. The `debug_assert!` above catches that only in
                    // debug builds.
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
            // SAFETY:
            // In order for the call to be safe, we need to ensure:
            // * `value` holds an initialized `T`.
            // * the write that stored it happens before this call.
            //
            // `initialized` read `value_set` as true with acquire ordering, so both hold (see
            // `get`).
            let value = unsafe { self.get_unchecked() };
            Ok(value)
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

                    // SAFETY:
                    // In order for the call to be safe, we need to ensure:
                    // * `value` holds an initialized `T`.
                    // * the write that stored it happens before this call.
                    //
                    // `acquire` fails only if the semaphore is closed. In tokio, only `set_value`
                    // closes it, after it stores the value, so the value is there, and the store
                    // happens before the close that this `acquire` saw. In Shuttle that does not
                    // always hold. `BatchSemaphore` also closes itself when a permit is released
                    // while `ExecutionState::should_stop()` is true (see
                    // `BatchSemaphore::release`), for example after the execution stopped or
                    // failed, or outside an execution. If that permit was the one of an unfinished
                    // `get_or_init` or `get_or_try_init`, the cell is still empty, and this reads
                    // a `T` that was never written. The `debug_assert!` above catches that only in
                    // debug builds.
                    let value = unsafe { self.get_unchecked() };
                    Ok(value)
                }
            }
        }
    }

    /// Takes the value from the cell, destroying the cell in the process.
    /// Returns `None` if the cell is empty.
    pub fn into_inner(mut self) -> Option<T> {
        if self.initialized_mut() {
            // Set to uninitialized for the destructor of `OnceCell` to work properly
            *self.value_set.get_mut() = false;

            // SAFETY:
            // In order for the read to be safe, we need to ensure:
            // * `value` holds an initialized `T`.
            // * nothing uses or drops that `T` in the cell after it is moved out.
            //
            // `value_set` was true, which it is only once `value` holds a `T` (see `Drop`).
            // `value_set` is false now, so the cell's `drop`, which runs when `self` goes out of
            // scope below, does not drop the `T` again. `self` is owned here, so nothing else uses
            // the cell.
            let value = unsafe { self.value.with(|ptr| ptr::read(ptr).assume_init()) };
            Some(value)
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

// SAFETY:
// In order for `OnceCell<T>` to be `Sync`, we need to ensure:
// * threads that share a `&OnceCell<T>` can share the `T`.
// * a `T` that one thread stores may move to another thread.
// * reads of `value` never race with its write.
// * no two OS threads use the cell's semaphore at the same time.
//
// `get` gives each thread a `&T`, so `T` must be `Sync`. `set` and `get_or_init` store a `T` that
// the calling thread made, and the thread that drops the cell drops that `T`, so `T` must be `Send`.
// The bound requires both.
// `value` is written once, by the holder of the only permit, before `value_set` is set with release
// ordering. Every read comes after an acquire load of `value_set` that sees true, except the reads
// in the `Err` arms of `get_or_init` and `get_or_try_init`. Their SAFETY comments say when those
// read a cell that was never written.
// The semaphore is this crate's `Semaphore`, which holds a `BatchSemaphore`, so the last property
// relies on the `Sync` impl of `BatchSemaphore` (see its SAFETY comment). That impl does not hold
// when two OS threads use one semaphore at the same time, for example two tests that run at the
// same time and share one cell through a `static`. So this impl does not hold then either.
unsafe impl<T: Sync + Send> Sync for OnceCell<T> {}

// `OnceCell<T>` is `Send` when `T` is, without an `unsafe impl` of its own, because each of its
// fields is `Send` then. This checks that it stays so, for every `T: Send`.
const _: () = {
    const fn assert_send<T: Send + ?Sized>() {}
    #[allow(dead_code)]
    const fn once_cell_is_send<T: Send>() {
        assert_send::<OnceCell<T>>();
    }
};

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
