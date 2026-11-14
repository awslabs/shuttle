//! The [`Guard`] that keeps a participant pinned, and the [`unprotected`] guard.

use crate::atomic::{Pointable, Shared};
use crate::collector::{Collector, Local};
use shuttle_engine::runtime::thread::switch;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

/// A function a guard was handed, which the model never calls (see the [crate documentation](crate)).
///
/// It is held until the execution ends, and then dropped without being called, which drops what it
/// captured.
pub(crate) struct Deferred {
    /// Never called; see the crate documentation.
    #[allow(dead_code)]
    call: Box<dyn FnOnce() + 'static>,
}

impl Deferred {
    /// # Safety
    ///
    /// `f` must not hold a reference to anything that is dropped before the execution ends, since it
    /// is held until then. [`Guard::defer_unchecked`]'s contract asks the caller for that, and
    /// [`Guard::defer`] takes `F: Send + 'static`, which gives it.
    pub(crate) unsafe fn new<'f, F: FnOnce() + 'f>(f: F) -> Self {
        let boxed: Box<dyn FnOnce() + 'f> = Box::new(f);
        Self {
            // Safety: the caller's promise above.
            call: unsafe { std::mem::transmute::<Box<dyn FnOnce() + 'f>, Box<dyn FnOnce() + 'static>>(boxed) },
        }
    }
}

/// A guard that keeps the current participant pinned, with the API of [`crossbeam_epoch::Guard`].
///
/// Pinning is reentrant, as in crossbeam: the participant is pinned when the first guard is created
/// and unpinned when the last one is dropped.
///
/// A guard is what lets an [`Atomic`](crate::Atomic) be loaded. Since the model never reclaims, a
/// guard protects nothing in particular; what it still does is take the scheduling points that
/// crossbeam's pinning takes, so that a loop that waits by pinning lets other tasks run.
///
/// [`crossbeam_epoch::Guard`]: https://docs.rs/crossbeam-epoch/0.9/crossbeam_epoch/struct.Guard.html
pub struct Guard {
    /// The participant this guard pins, or `None` for the [`unprotected`] guard.
    local: Option<Arc<Local>>,
    /// crossbeam's guard holds a raw pointer, so it is neither `Send` nor `Sync`.
    _marker: PhantomData<*mut ()>,
}

impl Guard {
    pub(crate) fn new(local: Arc<Local>) -> Self {
        Self {
            local: Some(local),
            _marker: PhantomData,
        }
    }

    /// The guard that protects nothing, which [`unprotected`] hands out.
    const fn unprotected() -> Self {
        Self {
            local: None,
            _marker: PhantomData,
        }
    }

    /// Stores a function so that it can be executed at some point after all currently pinned
    /// threads get unpinned.
    ///
    /// **This model never executes it**, unless this is an [`unprotected`] guard, where the function
    /// runs immediately, as in crossbeam. See the [crate documentation](crate).
    ///
    /// Not a scheduling point.
    pub fn defer<F, R>(&self, f: F)
    where
        F: FnOnce() -> R,
        F: Send + 'static,
    {
        // Safety: `f` is `Send + 'static`, which is more than `defer_unchecked` asks for.
        unsafe {
            self.defer_unchecked(f);
        }
    }

    /// Stores a function so that it can be executed at some point after all currently pinned
    /// threads get unpinned.
    ///
    /// **This model never executes it**, unless this is an [`unprotected`] guard, where the function
    /// runs immediately, as in crossbeam. See the [crate documentation](crate).
    ///
    /// Not a scheduling point.
    ///
    /// # Safety
    ///
    /// The given function must not hold reference onto the stack. It is highly recommended that the
    /// passed function is **always** marked with `move` in order to prevent accidental borrows.
    pub unsafe fn defer_unchecked<F, R>(&self, f: F)
    where
        F: FnOnce() -> R,
    {
        match &self.local {
            // Safety: the function is held until the execution ends, and this method's contract asks
            // the caller not to let it borrow anything shorter-lived.
            Some(local) => local.defer(unsafe { Deferred::new(move || drop(f())) }),
            None => drop(f()),
        }
    }

    /// Stores a destructor for an object so that it can be destroyed and deallocated at some point
    /// after all currently pinned threads get unpinned.
    ///
    /// **This model never destroys it**, unless this is an [`unprotected`] guard, where the object is
    /// destroyed immediately, as in crossbeam. See the [crate documentation](crate).
    ///
    /// Not a scheduling point.
    ///
    /// # Safety
    ///
    /// The object must not be reachable by other threads anymore, otherwise it might be still in use
    /// when the destructor runs.
    pub unsafe fn defer_destroy<T: ?Sized + Pointable>(&self, ptr: Shared<'_, T>) {
        // Safety: the caller promises that nothing else reaches the object.
        unsafe { self.defer_unchecked(move || ptr.into_owned()) }
    }

    /// Clears up the thread-local cache of deferred functions by executing them or moving into the
    /// global cache.
    ///
    /// **This model executes nothing** (see the [crate documentation](crate)). One scheduling point,
    /// where crossbeam's `flush` pushes its bag into the global queue and tries to advance the epoch;
    /// none for an [`unprotected`] guard, as in crossbeam.
    pub fn flush(&self) {
        if self.local.is_some() {
            switch();
        }
    }

    /// Unpins and then immediately re-pins the thread.
    ///
    /// This method is useful when you don't want delay the advancement of the global epoch by
    /// holding an old epoch. For safety, you should not maintain any guard-based reference across
    /// the call (the latter is enforced by `&mut self`).
    ///
    /// One scheduling point, where crossbeam stores a new epoch; none for an [`unprotected`] guard.
    pub fn repin(&mut self) {
        if let Some(local) = &self.local {
            local.repin();
        }
    }

    /// Temporarily unpins the thread, executes the given function and then re-pins the thread.
    ///
    /// This method is useful when you need to perform a long-running operation (e.g. sleeping) and
    /// don't need to maintain any guard-based reference across the call (the latter is enforced by
    /// `&mut self`).
    ///
    /// The thread is unpinned while `f` runs, as in crossbeam, so
    /// [`is_pinned`](crate::is_pinned) is false inside it if this was the participant's only guard.
    /// Two scheduling points, one before `f` and one after; none for an [`unprotected`] guard.
    pub fn repin_after<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        /// Repins the participant however `f` ends.
        struct Repin<'a>(Option<&'a Arc<Local>>);
        impl Drop for Repin<'_> {
            fn drop(&mut self) {
                if let Some(local) = self.0 {
                    local.pin_without_guard();
                }
            }
        }

        if let Some(local) = &self.local {
            local.unpin();
        }
        let _repin = Repin(self.local.as_ref());

        f()
    }

    /// Returns the `Collector` associated with this guard.
    ///
    /// This method is useful when you need to ensure that all guards used with a data structure come
    /// from the same collector. Returns `None` for an [`unprotected`] guard.
    pub fn collector(&self) -> Option<&Collector> {
        self.local.as_ref().map(|local| local.collector())
    }
}

impl Drop for Guard {
    #[inline]
    fn drop(&mut self) {
        if let Some(local) = &self.local {
            local.unpin();
        }
    }
}

impl fmt::Debug for Guard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Guard { .. }")
    }
}

/// Returns a reference to a dummy guard that allows unprotected access to [`Atomic`]s.
///
/// This guard should be used in special occasions only. Note that it doesn't actually keep any
/// thread pinned - it's just a fake guard that allows loading from [`Atomic`]s unsafely.
///
/// Note that calling [`defer`](Guard::defer) with a dummy guard will not defer the function -
/// it will just execute the function immediately, as in crossbeam.
///
/// It works anywhere, inside a Shuttle execution or not, and takes no scheduling point.
///
/// # Safety
///
/// Loading and dereferencing data from an [`Atomic`] using this guard is safe only if the
/// [`Atomic`] is not being concurrently modified by other threads.
///
/// [`Atomic`]: crate::Atomic
#[inline]
pub unsafe fn unprotected() -> &'static Guard {
    // An alternative solution would be `const { &Guard::unprotected() }`, which is a promotion of a
    // value whose type is not `Sync`, and so is rejected.
    struct GuardWrapper(Guard);
    // Safety: the guard holds nothing: it is `!Sync` only because crossbeam's holds a raw pointer.
    unsafe impl Sync for GuardWrapper {}
    static UNPROTECTED: GuardWrapper = GuardWrapper(Guard::unprotected());
    &UNPROTECTED.0
}
