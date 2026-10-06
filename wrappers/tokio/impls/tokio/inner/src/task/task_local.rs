//! Task-local storage: [`task_local!`](crate::task_local), [`LocalKey`] and [`TaskLocalFuture`].
//!
//! This is a port of tokio's `task/task_local.rs` (tokio 1.53). The API and the semantics are
//! tokio's. What changes is where a scope's value is kept while the scope runs; the places that
//! differ from tokio are marked with `SHUTTLE_CHANGES`.
//!
//! tokio moves the value of a scope into a `std::thread_local!` slot for the duration of each
//! poll of the scoped future (or of the `sync_scope` closure), and moves it back out afterwards.
//! That is sound in tokio, because an OS thread runs one task at a time, and a poll runs to
//! completion before the thread polls any other task. Neither holds under Shuttle: every Shuttle
//! task runs on the same OS thread, and a task can be switched out *in the middle of* a poll, at
//! any scheduling point (taking a lock, sending on a channel, spawning a task, and so on). With
//! tokio's implementation, the tasks that run in the meantime find the switched-out task's value
//! in the thread-local, so they
//!
//! * see a value they never set (a spawned task appears to inherit its parent's value),
//! * replace it with their own when they enter a scope, so the switched-out task finds *their*
//!   value when it resumes,
//! * move the values into the wrong scopes when their polls do not end in the opposite order they
//!   started in, which moves a value from one task to another for good, or leaves it in the
//!   thread-local after every scope has ended, and
//! * panic when they enter a scope while the switched-out task is inside `LocalKey::with`.
//!
//! A value can even outlive the execution. When Shuttle stops an execution while a task is in the
//! middle of a poll of a scope, it discards the task without unwinding it, so the value is never
//! moved back out, and the next execution starts out with it set.
//!
//! Here the slot is Shuttle's task-local storage instead (`shuttle::thread_local!`), so every
//! Shuttle task (async task or thread) has a slot of its own, and is the only one to see the values
//! it scopes, wherever it is switched out. A task's slot goes away with the task, so nothing
//! outlives an execution. Accessing a task-local is not a scheduling point, as it is not in tokio:
//! it is not visible to any other task.
//!
//! Whenever no Shuttle task is running, the slot is a plain `std::thread_local!` instead, which
//! is what tokio uses all the time. That is the case outside of a Shuttle test, where a
//! `LocalKey` therefore behaves exactly as tokio's does. It is also the case while Shuttle tears an
//! execution down and drops the tasks that have not finished, and while Shuttle updates its own
//! state (which is when it calls `tracing` subscribers for some of its own events). In all of these
//! only one thing runs at a time, so a single slot is as sound there as it is in tokio: scopes can
//! only nest, never interleave. A future that is dropped at teardown still sees the values of the
//! scopes it is in, as it does when a tokio runtime shuts down. There is one exception: if the task
//! was switched out in the middle of a poll of a scope, Shuttle first unwinds its stack, and the
//! destructors that run during that unwinding (which include those of the local variables of an
//! `async` block that is being polled) see no value.
//!
//! A `tracing` subscriber may read task-locals while it handles an event, as it can in tokio.

use pin_project_lite::pin_project;
use std::cell::{BorrowMutError, RefCell};
use std::error::Error;
use std::future::Future;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::{fmt, mem, thread};

/// Declares a new task-local key of type [`LocalKey`].
///
/// # Syntax
///
/// The macro wraps any number of static declarations and makes them local to the current task.
/// Publicity and attributes for each static is preserved. For example:
///
/// # Examples
///
/// ```
/// # use shuttle_tokio_impl_inner::task_local;
/// task_local! {
///     pub static ONE: u32;
///
///     #[allow(unused)]
///     static TWO: f32;
/// }
/// # fn main() {}
/// ```
///
/// See the [`LocalKey`] documentation for more information.
///
/// [`LocalKey`]: struct@crate::task::LocalKey
#[macro_export]
macro_rules! task_local {
     // empty (base case for the recursion)
    () => {};

    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty; $($rest:tt)*) => {
        $crate::__task_local_inner!($(#[$attr])* $vis $name, $t);
        $crate::task_local!($($rest)*);
    };

    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty) => {
        $crate::__task_local_inner!($(#[$attr])* $vis $name, $t);
    }
}

#[doc(hidden)]
#[macro_export]
macro_rules! __task_local_inner {
    ($(#[$attr:meta])* $vis:vis $name:ident, $t:ty) => {
        $(#[$attr])*
        $vis static $name: $crate::task::LocalKey<$t> = {
            // SHUTTLE_CHANGES: tokio declares a single `std::thread_local!` here. See `LocalKey`
            // for what each of these two slots is for.
            $crate::macros::support::__shuttle_thread_local! {
                static __TASK_SLOT: ::std::cell::RefCell<::std::option::Option<$t>> =
                    ::std::cell::RefCell::new(::std::option::Option::None);
            }
            ::std::thread_local! {
                static __FALLBACK_SLOT: ::std::cell::RefCell<::std::option::Option<$t>> =
                    const { ::std::cell::RefCell::new(::std::option::Option::None) };
            }

            $crate::task::LocalKey {
                task_slot: &__TASK_SLOT,
                fallback_slot: __FALLBACK_SLOT,
            }
        };
    };
}

/// A key for task-local data.
///
/// This type is generated by the [`task_local!`] macro.
///
/// Unlike [`std::thread::LocalKey`], `tokio::task::LocalKey` will
/// _not_ lazily initialize the value on first access. Instead, the
/// value is first initialized when the future containing
/// the task-local is first polled by a futures executor, like Tokio.
///
/// # Examples
///
/// ```
/// # async fn dox() {
/// shuttle_tokio_impl_inner::task_local! {
///     static NUMBER: u32;
/// }
///
/// NUMBER.scope(1, async move {
///     assert_eq!(NUMBER.get(), 1);
/// }).await;
///
/// NUMBER.scope(2, async move {
///     assert_eq!(NUMBER.get(), 2);
///
///     NUMBER.scope(3, async move {
///         assert_eq!(NUMBER.get(), 3);
///     }).await;
/// }).await;
/// # }
/// ```
///
/// [`std::thread::LocalKey`]: struct@std::thread::LocalKey
/// [`task_local!`]: ../macro.task_local.html
pub struct LocalKey<T: 'static> {
    // SHUTTLE_CHANGES: tokio has a single `std::thread::LocalKey` here.
    //
    // The slot that a scope's value is moved into while the running Shuttle task polls the scope.
    // Every Shuttle task has its own.
    #[doc(hidden)]
    pub task_slot: &'static shuttle::thread::LocalKey<RefCell<Option<T>>>,
    // The slot used instead whenever no Shuttle task is running: outside of a Shuttle test, while
    // Shuttle tears an execution down, and while Shuttle updates its own state (see the module
    // docs). It is what tokio uses all the time, and is as sound here as it is there, since only
    // one thing runs at a time in each of those cases.
    #[doc(hidden)]
    pub fallback_slot: thread::LocalKey<RefCell<Option<T>>>,
}

impl<T: 'static> LocalKey<T> {
    /// Sets a value `T` as the task-local value for the future `F`.
    ///
    /// On completion of `scope`, the task-local will be dropped.
    ///
    /// ### Panics
    ///
    /// If you poll the returned future inside a call to [`with`] or
    /// [`try_with`] on the same `LocalKey`, then the call to `poll` will panic.
    ///
    /// ### Examples
    ///
    /// ```
    /// # async fn dox() {
    /// shuttle_tokio_impl_inner::task_local! {
    ///     static NUMBER: u32;
    /// }
    ///
    /// NUMBER.scope(1, async move {
    ///     println!("task local value: {}", NUMBER.get());
    /// }).await;
    /// # }
    /// ```
    ///
    /// [`with`]: fn@Self::with
    /// [`try_with`]: fn@Self::try_with
    pub fn scope<F>(&'static self, value: T, f: F) -> TaskLocalFuture<T, F>
    where
        F: Future,
    {
        TaskLocalFuture {
            local: self,
            slot: Some(value),
            future: Some(f),
            _pinned: PhantomPinned,
        }
    }

    /// Sets a value `T` as the task-local value for the closure `F`.
    ///
    /// On completion of `sync_scope`, the task-local will be dropped.
    ///
    /// ### Panics
    ///
    /// This method panics if called inside a call to [`with`] or [`try_with`]
    /// on the same `LocalKey`.
    ///
    /// ### Examples
    ///
    /// ```
    /// # async fn dox() {
    /// shuttle_tokio_impl_inner::task_local! {
    ///     static NUMBER: u32;
    /// }
    ///
    /// NUMBER.sync_scope(1, || {
    ///     println!("task local value: {}", NUMBER.get());
    /// });
    /// # }
    /// ```
    ///
    /// [`with`]: fn@Self::with
    /// [`try_with`]: fn@Self::try_with
    #[track_caller]
    pub fn sync_scope<F, R>(&'static self, value: T, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let mut value = Some(value);
        match self.scope_inner(&mut value, f) {
            Ok(res) => res,
            Err(err) => err.panic(),
        }
    }

    fn scope_inner<F, R>(&'static self, slot: &mut Option<T>, f: F) -> Result<R, ScopeInnerErr>
    where
        F: FnOnce() -> R,
    {
        struct Guard<'a, T: 'static> {
            // SHUTTLE_CHANGES: tokio holds the `LocalKey` here and looks the slot up again on drop.
            // See below for why we hold on to the slot instead.
            cell: &'a RefCell<Option<T>>,
            slot: &'a mut Option<T>,
        }

        impl<T: 'static> Drop for Guard<'_, T> {
            fn drop(&mut self) {
                // This should not panic.
                //
                // We know that the RefCell was not borrowed before the call to
                // `scope_inner`, so the only way for this to panic is if the
                // closure has created but not destroyed a RefCell guard.
                // However, we never give user-code access to the guards, so
                // there's no way for user-code to forget to destroy a guard.
                //
                // SHUTTLE_CHANGES: The value goes back out of the slot it was moved into, not out
                // of a fresh lookup's. `with_slot` picks the slot by whether a task is running, and
                // that can change while `f` runs: if the execution ends while this task is switched
                // out inside `f` (blocked in the middle of a poll), `ExecutionState::cleanup`
                // unwinds the task's stack with no task running. A lookup would return the fallback
                // slot, leaving the value in the task's slot, and the `TaskLocalFuture` would drop
                // its future without it. The task's slot is still there then, as Shuttle drops a
                // task's stack before its storage. Covered by
                // `teardown::task_blocked_in_the_middle_of_a_poll_of_a_scope`.
                let mut ref_mut = self.cell.borrow_mut();
                mem::swap(self.slot, &mut *ref_mut);
            }
        }

        self.with_slot(|cell| {
            cell.try_borrow_mut()
                .map(|mut ref_mut| mem::swap(slot, &mut *ref_mut))?;

            let guard = Guard { cell, slot };

            let res = f();

            drop(guard);

            Ok(res)
        })?
    }

    // SHUTTLE_CHANGES: Added. tokio uses its one thread-local wherever this is called.
    /// Runs `f` on the slot that a scope's value is moved into: the running Shuttle task's own, or
    /// the fallback slot if no task is running (see the fields of `LocalKey`).
    ///
    /// Returns an `AccessError` if the slot has already been destroyed, which happens if this is
    /// called from the destructor of another task-local or thread-local value.
    fn with_slot<F, R>(&'static self, f: F) -> Result<R, AccessError>
    where
        F: FnOnce(&RefCell<Option<T>>) -> R,
    {
        // `try_get_current_task` is `None` in exactly the cases the fallback slot is for, and does
        // not panic in any of them. Checking it first also keeps us from calling into
        // `shuttle::thread::LocalKey` then, which would panic.
        let res = if shuttle::current::try_get_current_task().is_some() {
            self.task_slot.try_with(f).ok()
        } else {
            self.fallback_slot.try_with(f).ok()
        };

        res.ok_or(AccessError { _private: () })
    }

    /// Accesses the current task-local and runs the provided closure.
    ///
    /// # Panics
    ///
    /// This function will panic if the task local doesn't have a value set.
    #[track_caller]
    pub fn with<F, R>(&'static self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        match self.try_with(f) {
            Ok(res) => res,
            Err(_) => panic!("cannot access a task-local storage value without setting it first"),
        }
    }

    /// Accesses the current task-local and runs the provided closure.
    ///
    /// If the task-local with the associated key is not present, this
    /// method will return an `AccessError`. For a panicking variant,
    /// see `with`.
    pub fn try_with<F, R>(&'static self, f: F) -> Result<R, AccessError>
    where
        F: FnOnce(&T) -> R,
    {
        // If called after the thread-local storing the task-local is destroyed,
        // then we are outside of a closure where the task-local is set.
        //
        // Therefore, it is correct to return an AccessError if `try_with`
        // returns an error.
        let try_with_res = self.with_slot(|v| {
            // This call to `borrow` cannot panic because no user-defined code
            // runs while a `borrow_mut` call is active.
            v.borrow().as_ref().map(f)
        });

        match try_with_res {
            Ok(Some(res)) => Ok(res),
            Ok(None) | Err(_) => Err(AccessError { _private: () }),
        }
    }
}

impl<T: Clone + 'static> LocalKey<T> {
    /// Returns a copy of the task-local value
    /// if the task-local value implements `Clone`.
    ///
    /// # Panics
    ///
    /// This function will panic if the task local doesn't have a value set.
    #[track_caller]
    pub fn get(&'static self) -> T {
        self.with(|v| v.clone())
    }

    /// Returns a copy of the task-local value
    /// if the task-local value implements `Clone`.
    ///
    /// If the task-local with the associated key is not present, this
    /// method will return an `AccessError`. For a panicking variant,
    /// see `get`.
    pub fn try_get(&'static self) -> Result<T, AccessError> {
        self.try_with(|v| v.clone())
    }
}

impl<T: 'static> fmt::Debug for LocalKey<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("LocalKey { .. }")
    }
}

pin_project! {
    /// A future that sets a value `T` of a task local for the future `F` during
    /// its execution.
    ///
    /// The value of the task-local must be `'static` and will be dropped on the
    /// completion of the future.
    ///
    /// Created by the function [`LocalKey::scope`](self::LocalKey::scope).
    ///
    /// ### Examples
    ///
    /// ```
    /// # async fn dox() {
    /// shuttle_tokio_impl_inner::task_local! {
    ///     static NUMBER: u32;
    /// }
    ///
    /// NUMBER.scope(1, async move {
    ///     println!("task local value: {}", NUMBER.get());
    /// }).await;
    /// # }
    /// ```
    pub struct TaskLocalFuture<T, F>
    where
        T: 'static,
    {
        local: &'static LocalKey<T>,
        slot: Option<T>,
        #[pin]
        future: Option<F>,
        #[pin]
        _pinned: PhantomPinned,
    }

    impl<T: 'static, F> PinnedDrop for TaskLocalFuture<T, F> {
        fn drop(this: Pin<&mut Self>) {
            let this = this.project();
            if mem::needs_drop::<F>() && this.future.is_some() {
                // Drop the future while the task-local is set, if possible. Otherwise
                // the future is dropped normally when the `Option<F>` field drops.
                let mut future = this.future;
                let _ = this.local.scope_inner(this.slot, || {
                    future.set(None);
                });
            }
        }
    }
}

impl<T, F> TaskLocalFuture<T, F>
where
    T: 'static,
{
    /// Returns the value stored in the task local by this `TaskLocalFuture`.
    ///
    /// The function returns:
    ///
    /// * `Some(T)` if the task local value exists.
    /// * `None` if the task local value has already been taken.
    ///
    /// Note that this function attempts to take the task local value even if
    /// the future has not yet completed. In that case, the value will no longer
    /// be available via the task local after the call to `take_value`.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn dox() {
    /// shuttle_tokio_impl_inner::task_local! {
    ///     static KEY: u32;
    /// }
    ///
    /// let fut = KEY.scope(42, async {
    ///     // Do some async work
    /// });
    ///
    /// let mut pinned = Box::pin(fut);
    ///
    /// // Complete the TaskLocalFuture
    /// let _ = pinned.as_mut().await;
    ///
    /// // And here, we can take task local value
    /// let value = pinned.as_mut().take_value();
    ///
    /// assert_eq!(value, Some(42));
    /// # }
    /// ```
    pub fn take_value(self: Pin<&mut Self>) -> Option<T> {
        let this = self.project();
        this.slot.take()
    }
}

impl<T: 'static, F: Future> Future for TaskLocalFuture<T, F> {
    type Output = F::Output;

    #[track_caller]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let mut future_opt = this.future;

        let res = this
            .local
            .scope_inner(this.slot, || match future_opt.as_mut().as_pin_mut() {
                Some(fut) => {
                    let res = fut.poll(cx);
                    if res.is_ready() {
                        future_opt.set(None);
                    }
                    Some(res)
                }
                None => None,
            });

        match res {
            Ok(Some(res)) => res,
            Ok(None) => panic!("`TaskLocalFuture` polled after completion"),
            Err(err) => err.panic(),
        }
    }
}

impl<T: 'static, F> fmt::Debug for TaskLocalFuture<T, F>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        /// Format the Option without Some.
        struct TransparentOption<'a, T> {
            value: &'a Option<T>,
        }
        impl<T: fmt::Debug> fmt::Debug for TransparentOption<'_, T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self.value.as_ref() {
                    Some(value) => value.fmt(f),
                    // Hitting the None branch should not be possible.
                    None => f.pad("<missing>"),
                }
            }
        }

        f.debug_struct("TaskLocalFuture")
            .field("value", &TransparentOption { value: &self.slot })
            .finish()
    }
}

/// An error returned by [`LocalKey::try_with`](method@LocalKey::try_with).
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct AccessError {
    _private: (),
}

impl fmt::Debug for AccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessError").finish()
    }
}

impl fmt::Display for AccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt("task-local value not set", f)
    }
}

impl Error for AccessError {}

enum ScopeInnerErr {
    BorrowError,
    AccessError,
}

impl ScopeInnerErr {
    #[track_caller]
    fn panic(&self) -> ! {
        match self {
            Self::BorrowError => panic!("cannot enter a task-local scope while the task-local storage is borrowed"),
            Self::AccessError => {
                panic!("cannot enter a task-local scope during or after destruction of the underlying thread-local")
            }
        }
    }
}

impl From<BorrowMutError> for ScopeInnerErr {
    fn from(_: BorrowMutError) -> Self {
        Self::BorrowError
    }
}

// SHUTTLE_CHANGES: tokio converts from `std::thread::AccessError`; `with_slot` reports both kinds
// of slot as this crate's `AccessError`.
impl From<AccessError> for ScopeInnerErr {
    fn from(_: AccessError) -> Self {
        Self::AccessError
    }
}
