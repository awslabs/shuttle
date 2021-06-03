use crate::sync::atomic::Atomic;
use std::sync::atomic::Ordering;

/// A boolean type which can be safely shared between threads.
pub struct AtomicBool {
    inner: Atomic<bool>,
}

impl Default for AtomicBool {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl From<bool> for AtomicBool {
    fn from(b: bool) -> Self {
        Self::new(b)
    }
}

impl std::fmt::Debug for AtomicBool {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        std::fmt::Debug::fmt(unsafe { &self.raw_load() }, f)
    }
}

impl AtomicBool {
    /// Creates a new atomic boolean.
    pub const fn new(v: bool) -> Self {
        Self { inner: Atomic::new(v) }
    }

    /// Returns a mutable reference to the underlying boolean.
    pub fn get_mut(&mut self) -> &mut bool {
        self.inner.get_mut()
    }

    /// Consumes the atomic and returns the contained value.
    pub fn into_inner(self) -> bool {
        self.inner.into_inner()
    }

    /// Loads a value from the atomic boolean.
    pub fn load(&self, order: Ordering) -> bool {
        self.inner.load(order)
    }

    /// Stores a value into the atomic boolean.
    pub fn store(&self, val: bool, order: Ordering) {
        self.inner.store(val, order)
    }

    /// Stores a value into the atomic boolean, returning the previous value.
    pub fn swap(&self, val: bool, order: Ordering) -> bool {
        self.inner.swap(val, order)
    }

    /// Fetches the value, and applies a function to it that returns an optional new value.
    /// Returns a `Result` of `Ok(previous_value)` if the function returned `Some(_)`, else
    /// `Err(previous_value)`.
    pub fn fetch_update<F>(&self, set_order: Ordering, fetch_order: Ordering, f: F) -> Result<bool, bool>
    where
        F: FnMut(bool) -> Option<bool>,
    {
        self.inner.fetch_update(set_order, fetch_order, f)
    }

    /// Stores a value into the atomic boolean if the current value is the same as the
    /// `current` value.
    #[deprecated(since = "0.0.6", note = "Use `compare_exchange` or `compare_exchange_weak` instead")]
    pub fn compare_and_swap(&self, current: bool, new: bool, order: Ordering) -> bool {
        match self.compare_exchange(current, new, order, order) {
            Ok(v) => v,
            Err(v) => v,
        }
    }

    /// Stores a value into the atomic boolean if the current value is the same as the
    /// `current` value.
    ///
    /// The return value is a result indicating whether the new value was written and
    /// containing the previous value. On success this value is guaranteed to be equal to
    /// `current`.
    pub fn compare_exchange(
        &self,
        current: bool,
        new: bool,
        success: Ordering,
        failure: Ordering,
    ) -> Result<bool, bool> {
        self.fetch_update(success, failure, |val| (val == current).then(|| new))
    }

    /// Stores a value into the atomic boolean if the current value is the same as the
    /// `current` value.
    ///
    /// Unlike [`AtomicBool::compare_exchange`], this function is allowed to spuriously fail
    /// even when the comparison succeeds, which can result in more efficient code on some
    /// platforms. The return value is a result indicating whether the new value was written
    /// and containing the previous value.
    // TODO actually produce spurious failures
    pub fn compare_exchange_weak(
        &self,
        current: bool,
        new: bool,
        success: Ordering,
        failure: Ordering,
    ) -> Result<bool, bool> {
        self.compare_exchange(current, new, success, failure)
    }

    /// Logical "and" with the current value. Returns the previous value.
    pub fn fetch_and(&self, val: bool, order: Ordering) -> bool {
        self.fetch_update(order, order, |old| Some(old & val)).unwrap()
    }

    /// Logical "nand" with the current value. Returns the previous value.
    pub fn fetch_nand(&self, val: bool, order: Ordering) -> bool {
        self.fetch_update(order, order, |old| Some(!(old & val))).unwrap()
    }

    /// Logical "or" with the current value. Returns the previous value.
    pub fn fetch_or(&self, val: bool, order: Ordering) -> bool {
        self.fetch_update(order, order, |old| Some(old | val)).unwrap()
    }

    /// Logical "xor" with the current value. Returns the previous value.
    pub fn fetch_xor(&self, val: bool, order: Ordering) -> bool {
        self.fetch_update(order, order, |old| Some(old ^ val)).unwrap()
    }

    /// Load the atomic value directly without triggering any Shuttle context switches.
    ///
    /// # Safety
    ///
    /// Shuttle does not consider potential concurrent interleavings of this function call,
    /// and so it should be used when those interleavings aren't important (primarily in
    /// debugging scenarios where we might want to just print this atomic's value).
    pub unsafe fn raw_load(&self) -> bool {
        self.inner.raw_load()
    }
}
