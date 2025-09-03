use crate::sync::atomic::Atomic;
#[cfg(test)]
use crate::sync::TypedResourceSignature;
use std::{sync::atomic::Ordering};

/// A raw pointer type which can be safely shared between threads.
pub struct AtomicPtr<T> {
    inner: Atomic<*mut T>,
}

impl<T> Default for AtomicPtr<T> {
    fn default() -> Self {
        Self::new(std::ptr::null_mut())
    }
}

impl<T> From<*mut T> for AtomicPtr<T> {
    fn from(p: *mut T) -> Self {
        Self::new(p)
    }
}

impl<T> std::fmt::Debug for AtomicPtr<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        std::fmt::Debug::fmt(unsafe { &self.raw_load() }, f)
    }
}

// Atomic operations make it safe to Send + Sync this shared raw pointer (establishing the safety of
// *using* the raw pointer is still up to the caller)
unsafe impl<T> Send for AtomicPtr<T> {}
unsafe impl<T> Sync for AtomicPtr<T> {}

impl<T> AtomicPtr<T> {
    /// Creates a new `AtomicPtr`.
    #[track_caller]
    pub const fn new(v: *mut T) -> Self {
        Self {
            inner: Atomic::new(v),
        }
    }

    /// Returns a mutable reference to the underlying pointer.
    pub fn get_mut(&mut self) -> &mut *mut T {
        self.inner.get_mut()
    }

    /// Consumes the atomic and returns the contained value.
    pub fn into_inner(self) -> *mut T {
        self.inner.into_inner()
    }

    /// Loads a value from the pointer.
    pub fn load(&self, order: Ordering) -> *mut T {
        self.inner.load(order)
    }

    /// Stores a value into the pointer.
    pub fn store(&self, val: *mut T, order: Ordering) {
        self.inner.store(val, order)
    }

    /// Stores a value into the atomic pointer, returning the previous value.
    pub fn swap(&self, val: *mut T, order: Ordering) -> *mut T {
        self.inner.swap(val, order)
    }

    /// Fetches the value, and applies a function to it that returns an optional new value.
    /// Returns a `Result` of `Ok(previous_value)` if the function returned `Some(_)`, else
    /// `Err(previous_value)`.
    pub fn fetch_update<F>(&self, set_order: Ordering, fetch_order: Ordering, f: F) -> Result<*mut T, *mut T>
    where
        F: FnMut(*mut T) -> Option<*mut T>,
    {
        self.inner.fetch_update(set_order, fetch_order, f)
    }

    /// Stores a value into the atomic pointer if the current value is the same as the
    /// `current` value.
    #[deprecated(since = "0.0.6", note = "Use `compare_exchange` or `compare_exchange_weak` instead")]
    pub fn compare_and_swap(&self, current: *mut T, new: *mut T, order: Ordering) -> *mut T {
        match self.compare_exchange(current, new, order, order) {
            Ok(v) => v,
            Err(v) => v,
        }
    }

    /// Stores a value into the atomic pointer if the current value is the same as the
    /// `current` value.
    ///
    /// The return value is a result indicating whether the new value was written and
    /// containing the previous value. On success this value is guaranteed to be equal to
    /// `current`.
    pub fn compare_exchange(
        &self,
        current: *mut T,
        new: *mut T,
        success: Ordering,
        failure: Ordering,
    ) -> Result<*mut T, *mut T> {
        self.fetch_update(success, failure, |val| (val == current).then_some(new))
    }

    /// Stores a value into the atomic pointer if the current value is the same as the
    /// `current` value.
    ///
    /// Unlike [`AtomicPtr::compare_exchange`], this function is allowed to spuriously fail
    /// even when the comparison succeeds, which can result in more efficient code on some
    /// platforms. The return value is a result indicating whether the new value was written
    /// and containing the previous value.
    // TODO actually produce spurious failures
    pub fn compare_exchange_weak(
        &self,
        current: *mut T,
        new: *mut T,
        success: Ordering,
        failure: Ordering,
    ) -> Result<*mut T, *mut T> {
        self.compare_exchange(current, new, success, failure)
    }

    /// Load the atomic value directly without triggering any Shuttle context switches.
    ///
    /// # Safety
    ///
    /// Shuttle does not consider potential concurrent interleavings of this function call,
    /// and so it should be used when those interleavings aren't important (primarily in
    /// debugging scenarios where we might want to just print this atomic's value).
    pub unsafe fn raw_load(&self) -> *mut T {
        self.inner.raw_load()
    }

    #[cfg(test)]
    pub(crate) fn signature(&self) -> TypedResourceSignature {
        self.inner.signature()
    }
}
