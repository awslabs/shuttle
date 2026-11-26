//! Synchronization primitives for one-time evaluation.
//!
//! Under Shuttle the status word is spin's (incomplete, running, complete or panicked), and the
//! value lives beside it in the running execution, so a `static` [`Once`] is incomplete again at
//! the start of every execution, as Shuttle's own `Once` and `lazy_static!` are.

use crate::state::{Core, State, release_after_switch};
use crate::{RelaxStrategy, Spin};
use shuttle_engine::runtime::thread;
use std::cell::{Cell, UnsafeCell};
use std::fmt;
use std::marker::PhantomData;
use std::mem::{ManuallyDrop, MaybeUninit};

const INCOMPLETE: usize = 0x00;
const RUNNING: usize = 0x01;
const COMPLETE: usize = 0x02;
const PANICKED: usize = 0x03;

/// A primitive that provides lazy one-time initialization.
///
/// Unlike its `std::sync` equivalent, this is generalized such that the closure returns a value to
/// be stored by the [`Once`] (`std::sync::Once` can be trivially emulated with `Once`).
///
/// Because [`Once::new`] is `const`, this primitive may be used to safely initialize statics.
///
/// # Under Shuttle
///
/// The value of a `Once` made with [`Once::new`] lives in the running execution: every execution
/// starts with the `Once` incomplete, an initializer runs in each, and the value is dropped at the
/// end of the execution, unless the `Once` is dropped before. So a `static` `Once` does not carry
/// a value from one execution into the next, which would stop a failing schedule from replaying.
/// A `Once` made with [`Once::initialized`] is complete in every execution.
///
/// [`call_once`](Self::call_once) and [`try_call_once`](Self::try_call_once) on a complete `Once`
/// take no scheduling point. Otherwise they take one before they claim the initialization, and the
/// winner takes another before it publishes the value, so every task can win the race. A task that
/// finds another one initializing blocks until it is done, and so do [`wait`](Self::wait) and
/// [`poll`](Self::poll). [`get`](Self::get) and [`is_completed`](Self::is_completed) take no
/// scheduling point. These are the scheduling points of Shuttle's `OnceLock`.
///
/// Storing a value requires `T: 'static`, as values in an execution's storage do: `call_once`
/// and `try_call_once` carry that bound, which spin's do not.
pub struct Once<T = (), R = Spin> {
    phantom: PhantomData<R>,
    core: Core,
    /// The value of a `Once` made by [`Once::initialized`], which is complete in every execution.
    inline: UnsafeCell<MaybeUninit<T>>,
    initialized: bool,
}

impl<T, R> Default for Once<T, R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: fmt::Debug, R> fmt::Debug for Once<T, R> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut d = f.debug_tuple("Once");
        let d = if let Some(x) = self.get() {
            d.field(&x)
        } else {
            d.field(&format_args!("<uninit>"))
        };
        d.finish()
    }
}

// Same unsafe impls as `std::sync::RwLock`, because this also allows for concurrent reads.
unsafe impl<T: Send + Sync, R> Sync for Once<T, R> {}
unsafe impl<T: Send, R> Send for Once<T, R> {}

/// Where a `Once` made by [`Once::new`] keeps its value in an execution, beside the status word. It
/// does not name the value's type, so that it can be found where the type is not `'static` (in
/// [`Once`]'s `Drop`). Only the methods that store a value, which require `T: 'static`, make it
/// complete, so only a `'static` value is ever dropped by execution teardown.
pub(crate) struct Slot {
    /// A `Box<MaybeUninit<T>>`, allocated with the slot so that [`Once::as_mut_ptr`] has an
    /// address to return.
    value: *mut (),
    /// Whether `value` holds a value.
    complete: Cell<bool>,
    /// Drops `value`'s value, if `complete`, and frees it.
    free: unsafe fn(*mut (), bool),
}

impl Slot {
    fn new<T>() -> Self {
        unsafe fn free<T>(value: *mut (), complete: bool) {
            // Safety: `value` came from `Box::into_raw` of a `Box<MaybeUninit<T>>`, and holds a
            // value if `complete`.
            let mut value = unsafe { Box::from_raw(value as *mut MaybeUninit<T>) };
            if complete {
                // Safety: as above.
                unsafe { value.assume_init_drop() };
            }
        }
        Self {
            value: Box::into_raw(Box::new(MaybeUninit::<T>::uninit())) as *mut (),
            complete: Cell::new(false),
            free: free::<T>,
        }
    }

    /// The value's address. Only the `Once` that made the slot, whose type is `T`, calls this.
    fn ptr<T>(&self) -> *mut T {
        self.value as *mut T
    }

    /// Drops the value, if the slot holds one.
    fn clear<T>(&self) {
        if self.complete.replace(false) {
            // Safety: the slot holds a value of the `Once`'s type.
            unsafe { std::ptr::drop_in_place(self.ptr::<T>()) };
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        // Safety: `free` is the slot's own, for the type `value` was allocated with.
        unsafe { (self.free)(self.value, self.complete.get()) };
    }
}

impl<T, R: RelaxStrategy> Once<T, R> {
    /// Performs an initialization routine once and only once. The given closure will be executed
    /// if this is the first time `call_once` has been called, and otherwise the routine will *not*
    /// be invoked.
    ///
    /// This method will block the calling task if another initialization routine is currently
    /// running.
    ///
    /// When this function returns, it is guaranteed that some initialization has run and completed
    /// (it may not be the closure specified). The returned pointer will point to the result from
    /// the closure that was run.
    ///
    /// # Panics
    ///
    /// This function will panic if the [`Once`] previously panicked while attempting to initialize.
    /// This is similar to the poisoning behaviour of `std::sync`'s primitives.
    pub fn call_once<F: FnOnce() -> T>(&self, f: F) -> &T
    where
        T: 'static,
    {
        match self.try_call_once(|| Ok::<T, core::convert::Infallible>(f())) {
            Ok(x) => x,
            Err(void) => match void {},
        }
    }

    /// This method is similar to `call_once`, but allows the given closure to fail, and lets the
    /// `Once` in a uninitialized state if it does.
    ///
    /// This method will block the calling task if another initialization routine is currently
    /// running.
    ///
    /// When this function returns without error, it is guaranteed that some initialization has run
    /// and completed (it may not be the closure specified). The returned reference will point to
    /// the result from the closure that was run.
    ///
    /// # Panics
    ///
    /// This function will panic if the [`Once`] previously panicked while attempting to initialize.
    /// This is similar to the poisoning behaviour of `std::sync`'s primitives.
    pub fn try_call_once<F: FnOnce() -> Result<T, E>, E>(&self, f: F) -> Result<&T, E>
    where
        T: 'static,
    {
        if let Some(value) = self.get() {
            Ok(value)
        } else {
            self.try_call_once_slow(f)
        }
    }

    #[cold]
    fn try_call_once_slow<F: FnOnce() -> Result<T, E>, E>(&self, f: F) -> Result<&T, E>
    where
        T: 'static,
    {
        thread::switch();
        let state = self.state();
        loop {
            state.observe();
            match state.word.get() {
                INCOMPLETE => {
                    state.word.set(RUNNING);
                    state.publish();
                    break;
                }
                PANICKED => panic!("Once panicked"),
                RUNNING => match self.poll_state(state) {
                    Some(v) => return Ok(v),
                    None => continue,
                },
                _ => {
                    // The status is Complete.
                    return Ok(self.value(state));
                }
            }
        }

        // We use a guard (Finish) to catch panics caused by builder: the Once is then poisoned,
        // and so is any task that waits for it.
        struct Finish<'a>(&'a State<Slot>);
        impl Drop for Finish<'_> {
            fn drop(&mut self) {
                let state = self.0;
                release_after_switch(|| state.release(|state| state.word.set(PANICKED)));
            }
        }
        let finish = Finish(state);
        let val = match f() {
            Ok(val) => val,
            Err(err) => {
                // If an error occurs, clean up everything and leave.
                core::mem::forget(finish);
                release_after_switch(|| state.release(|state| state.word.set(INCOMPLETE)));
                return Err(err);
            }
        };
        core::mem::forget(finish);

        let slot = &state.extra;
        // Safety: we won the initialization, so nothing else reaches the slot until the status
        // says it is complete.
        unsafe { slot.ptr::<T>().write(val) };
        slot.complete.set(true);
        // The publication is the step that lets the tasks that wait go on.
        release_after_switch(|| state.release(|state| state.word.set(COMPLETE)));
        Ok(self.value(state))
    }

    /// Spins until the [`Once`] contains a value.
    ///
    /// Note that in releases prior to `0.7`, this function had the behaviour of [`Once::poll`].
    ///
    /// Under Shuttle the task blocks until the `Once` is initialized; if no task ever initializes
    /// it, Shuttle reports a deadlock.
    ///
    /// # Panics
    ///
    /// This function will panic if the [`Once`] previously panicked while attempting to initialize.
    /// This is similar to the poisoning behaviour of `std::sync`'s primitives.
    pub fn wait(&self) -> &T {
        if self.initialized {
            return self.inline();
        }
        let state = self.state();
        loop {
            match self.poll_state(state) {
                Some(x) => break x,
                None => state.wait(),
            }
        }
    }

    /// Like [`Once::get`], but will spin if the [`Once`] is in the process of being initialized.
    /// If initialization has not even begun, `None` will be returned.
    ///
    /// Note that in releases prior to `0.7`, this function was named `wait`.
    ///
    /// # Panics
    ///
    /// This function will panic if the [`Once`] previously panicked while attempting to initialize.
    /// This is similar to the poisoning behaviour of `std::sync`'s primitives.
    pub fn poll(&self) -> Option<&T> {
        if self.initialized {
            return Some(self.inline());
        }
        let state = self.state();
        self.poll_state(state)
    }

    /// `poll` on this execution's state: blocks while another task initializes the `Once`.
    fn poll_state<'a>(&'a self, state: &'a State<Slot>) -> Option<&'a T> {
        loop {
            state.observe();
            match state.word.get() {
                INCOMPLETE => return None,
                RUNNING => state.wait(),
                COMPLETE => return Some(self.value(state)),
                _ => panic!("Once previously poisoned by a panicked"),
            }
        }
    }
}

impl<T, R> Once<T, R> {
    /// Initialization constant of [`Once`].
    #[allow(clippy::declare_interior_mutable_const)]
    pub const INIT: Self = Self {
        phantom: PhantomData,
        core: Core::new(),
        inline: UnsafeCell::new(MaybeUninit::uninit()),
        initialized: false,
    };

    /// Creates a new [`Once`].
    pub const fn new() -> Self {
        Self::INIT
    }

    /// Creates a new initialized [`Once`].
    pub const fn initialized(data: T) -> Self {
        Self {
            phantom: PhantomData,
            core: Core::new(),
            inline: UnsafeCell::new(MaybeUninit::new(data)),
            initialized: true,
        }
    }

    /// Retrieve a pointer to the inner data.
    ///
    /// While this method itself is safe, accessing the pointer before the [`Once`] has been
    /// initialized is UB, unless this method has already been written to from a pointer coming
    /// from this method.
    ///
    /// Under Shuttle the value of a `Once` made with [`Once::new`] lives in the running execution,
    /// so the pointer is valid for this execution only, and this panics outside of an execution.
    pub fn as_mut_ptr(&self) -> *mut T {
        if self.initialized {
            // SAFETY:
            // * MaybeUninit<T> always has exactly the same layout as T
            self.inline.get().cast::<T>()
        } else {
            self.state().extra.ptr::<T>()
        }
    }

    /// This execution's state of a `Once` made by [`Once::new`].
    fn state(&self) -> &State<Slot> {
        self.core.state(|| State::new(INCOMPLETE, Slot::new::<T>())).0
    }

    /// The value of a `Once` made by [`Once::initialized`].
    fn inline(&self) -> &T {
        // Safety: an initialized `Once`'s value never changes while it is borrowed.
        unsafe { (*self.inline.get()).assume_init_ref() }
    }

    /// The value in this execution, which the caller has seen complete.
    fn value<'a>(&'a self, state: &'a State<Slot>) -> &'a T {
        // Safety: a complete value never changes, and is moved out only through `&mut self`. The
        // reference outlives the execution only if the `Once` is a `static`, which is the
        // promotion `shuttle::lazy_static!` makes.
        unsafe { &*state.extra.ptr::<T>() }
    }

    /// Returns a reference to the inner value if the [`Once`] has been initialized.
    ///
    /// Under Shuttle this is not a scheduling point.
    pub fn get(&self) -> Option<&T> {
        if self.initialized {
            return Some(self.inline());
        }
        let state = self.core.existing_state::<Slot>()?;
        state.observe();
        if state.word.get() == COMPLETE {
            Some(self.value(state))
        } else {
            None
        }
    }

    /// Returns a reference to the inner value on the unchecked assumption that the [`Once`] has
    /// been initialized.
    ///
    /// # Safety
    ///
    /// This is *extremely* unsafe if the `Once` has not already been initialized because a
    /// reference to uninitialized memory will be returned, immediately triggering undefined
    /// behaviour (even if the reference goes unused). However, this can be useful in some
    /// instances for exposing the `Once` to FFI or when the overhead of atomically checking
    /// initialization is unacceptable and the `Once` has already been initialized.
    pub unsafe fn get_unchecked(&self) -> &T {
        match self.get() {
            Some(value) => value,
            None => {
                debug_assert!(
                    false,
                    "Attempted to access an uninitialized Once. If this was run without debug checks, this would be \
                     undefined behaviour. This is a serious bug and you must fix it.",
                );
                // Safety: the caller's.
                unsafe { &*self.as_mut_ptr() }
            }
        }
    }

    /// Returns a mutable reference to the inner value if the [`Once`] has been initialized.
    ///
    /// Because this method requires a mutable reference to the [`Once`], no synchronization
    /// overhead is required to access the inner value. In effect, it is zero-cost.
    pub fn get_mut(&mut self) -> Option<&mut T> {
        if self.initialized {
            // Safety: an initialized `Once` holds a value.
            return Some(unsafe { self.inline.get_mut().assume_init_mut() });
        }
        let state = self.core.existing_state::<Slot>()?;
        if state.word.get() == COMPLETE {
            // Safety: the `&mut` borrow rules out any other reference to the value.
            Some(unsafe { &mut *state.extra.ptr::<T>() })
        } else {
            None
        }
    }

    /// Returns a mutable reference to the inner value
    ///
    /// # Safety
    ///
    /// This is *extremely* unsafe if the `Once` has not already been initialized because a
    /// reference to uninitialized memory will be returned, immediately triggering undefined
    /// behaviour (even if the reference goes unused). However, this can be useful in some
    /// instances for exposing the `Once` to FFI or when the overhead of atomically checking
    /// initialization is unacceptable and the `Once` has already been initialized.
    pub unsafe fn get_mut_unchecked(&mut self) -> &mut T {
        let ptr = self.as_mut_ptr();
        debug_assert!(
            self.get_mut().is_some(),
            "Attempted to access an unintialized Once.  If this was to run without debug checks, this would be \
             undefined behavior.  This is a serious bug and you must fix it.",
        );
        // Safety: the caller's.
        unsafe { &mut *ptr }
    }

    /// Returns a the inner value if the [`Once`] has been initialized.
    ///
    /// Because this method requires ownership of the [`Once`], no synchronization overhead is
    /// required to access the inner value. In effect, it is zero-cost.
    pub fn try_into_inner(self) -> Option<T> {
        let mut this = ManuallyDrop::new(self);
        if this.initialized {
            // Safety: an initialized `Once` holds a value, and `this` is never dropped.
            return Some(unsafe { this.inline.get_mut().assume_init_read() });
        }
        let state = this.core.existing_state::<Slot>()?;
        if state.word.get() != COMPLETE {
            return None;
        }
        let slot = &state.extra;
        slot.complete.set(false);
        state.word.set(INCOMPLETE);
        // Safety: the slot held a value of this type, which it no longer claims.
        Some(unsafe { slot.ptr::<T>().read() })
    }

    /// Returns a the inner value if the [`Once`] has been initialized.
    ///
    /// # Safety
    ///
    /// This is *extremely* unsafe if the `Once` has not already been initialized because
    /// uninitialized memory will be read, immediately triggering undefined behavior. This can be
    /// useful, if `Once` has already been initialized, and you want to bypass an option check.
    pub unsafe fn into_inner_unchecked(self) -> T {
        match self.try_into_inner() {
            Some(value) => value,
            None => {
                debug_assert!(
                    false,
                    "Attempted to access an unintialized Once.  If this was to run without debug checks, this would \
                     be undefined behavior.  This is a serious bug and you must fix it.",
                );
                // Safety: the caller's.
                unsafe { std::hint::unreachable_unchecked() }
            }
        }
    }

    /// Checks whether the value has been initialized.
    ///
    /// This is done using [`Acquire`](core::sync::atomic::Ordering::Acquire) ordering, and
    /// therefore it is safe to access the value directly via
    /// [`get_unchecked`](Self::get_unchecked) if this returns true.
    ///
    /// Under Shuttle this is not a scheduling point.
    pub fn is_completed(&self) -> bool {
        self.get().is_some()
    }
}

impl<T, R> From<T> for Once<T, R> {
    fn from(data: T) -> Self {
        Self::initialized(data)
    }
}

impl<T, R> Drop for Once<T, R> {
    fn drop(&mut self) {
        if self.initialized {
            // Safety: an initialized `Once` holds a value, which nothing uses again.
            unsafe { self.inline.get_mut().assume_init_drop() };
        } else if let Some(state) = self.core.existing_state::<Slot>() {
            // The value of this execution goes with the `Once`, as spin's does. Outside of the
            // execution, or while Shuttle tears it down, the slot is dropped with the execution.
            state.extra.clear::<T>();
            state.word.set(INCOMPLETE);
        }
    }
}
