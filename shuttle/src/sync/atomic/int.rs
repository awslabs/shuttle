use crate::sync::atomic::Atomic;
use std::sync::atomic::Ordering;

macro_rules! atomic_int {
    ($name:ident, $int_type:ty) => {
        /// An integer type which can be safely shared between threads.
        pub struct $name {
            inner: Atomic<$int_type>,
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new(Default::default())
            }
        }

        impl From<$int_type> for $name {
            fn from(v: $int_type) -> Self {
                Self::new(v)
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                std::fmt::Debug::fmt(unsafe { &self.raw_load() }, f)
            }
        }

        impl $name {
            /// Creates a new atomic integer.
            #[track_caller]
            pub const fn new(v: $int_type) -> Self {
                Self {
                    inner: Atomic::new(v),
                }
            }

            /// Returns a mutable reference to the underlying integer.
            pub fn get_mut(&mut self) -> &mut $int_type {
                self.inner.get_mut()
            }

            /// Consumes the atomic and returns the contained value.
            pub fn into_inner(self) -> $int_type {
                self.inner.into_inner()
            }

            /// Loads a value from the atomic integer.
            #[track_caller]
            pub fn load(&self, order: Ordering) -> $int_type {
                self.inner.load(order)
            }

            /// Stores a value into the atomic integer.
            #[track_caller]
            pub fn store(&self, val: $int_type, order: Ordering) {
                self.inner.store(val, order)
            }

            /// Stores a value into the atomic integer, returning the previous value.
            #[track_caller]
            pub fn swap(&self, val: $int_type, order: Ordering) -> $int_type {
                self.inner.swap(val, order)
            }

            /// Fetches the value, and applies a function to it that returns an optional new value.
            /// Returns a `Result` of `Ok(previous_value)` if the function returned `Some(_)`, else
            /// `Err(previous_value)`.
            #[track_caller]
            pub fn fetch_update<F>(
                &self,
                set_order: Ordering,
                fetch_order: Ordering,
                f: F,
            ) -> Result<$int_type, $int_type>
            where
                F: FnMut($int_type) -> Option<$int_type>,
            {
                self.inner.fetch_update(set_order, fetch_order, f)
            }

            /// Stores a value into the atomic integer if the current value is the same as the
            /// `current` value.
            #[deprecated(
                since = "0.0.6",
                note = "Use `compare_exchange` or `compare_exchange_weak` instead"
            )]
            #[track_caller]
            pub fn compare_and_swap(&self, current: $int_type, new: $int_type, order: Ordering) -> $int_type {
                match self.compare_exchange(current, new, order, order) {
                    Ok(v) => v,
                    Err(v) => v,
                }
            }

            /// Stores a value into the atomic integer if the current value is the same as the
            /// `current` value.
            ///
            /// The return value is a result indicating whether the new value was written and
            /// containing the previous value. On success this value is guaranteed to be equal to
            /// `current`.
            #[track_caller]
            pub fn compare_exchange(
                &self,
                current: $int_type,
                new: $int_type,
                success: Ordering,
                failure: Ordering,
            ) -> Result<$int_type, $int_type> {
                self.fetch_update(success, failure, |val| (val == current).then(|| new))
            }

            /// Stores a value into the atomic integer if the current value is the same as the
            /// `current` value.
            ///
            /// Unlike `compare_exchange`, this function is allowed to spuriously fail even when
            /// the comparison succeeds, which can result in more efficient code on some platforms.
            /// The return value is a result indicating whether the new value was written and
            /// containing the previous value.
            // TODO actually produce spurious failures
            #[track_caller]
            pub fn compare_exchange_weak(
                &self,
                current: $int_type,
                new: $int_type,
                success: Ordering,
                failure: Ordering,
            ) -> Result<$int_type, $int_type> {
                self.compare_exchange(current, new, success, failure)
            }

            /// Adds to the current value, returning the previous value.
            ///
            /// This operation wraps around on overflow.
            #[track_caller]
            pub fn fetch_add(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(old.wrapping_add(val)))
                    .unwrap()
            }

            /// Subtracts from the current value, returning the previous value.
            ///
            /// This operation wraps around on overflow.
            #[track_caller]
            pub fn fetch_sub(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(old.wrapping_sub(val)))
                    .unwrap()
            }

            /// Bitwise "and" with the current value. Returns the previous value.
            #[track_caller]
            pub fn fetch_and(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(old & val)).unwrap()
            }

            /// Bitwise "nand" with the current value. Returns the previous value.
            #[track_caller]
            pub fn fetch_nand(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(!(old & val))).unwrap()
            }

            /// Bitwise "or" with the current value. Returns the previous value.
            #[track_caller]
            pub fn fetch_or(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(old | val)).unwrap()
            }

            /// Bitwise "xor" with the current value. Returns the previous value.
            #[track_caller]
            pub fn fetch_xor(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(old ^ val)).unwrap()
            }

            /// Maximum with the current value. Returns the previous value.
            #[track_caller]
            pub fn fetch_max(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(old.max(val))).unwrap()
            }

            /// Minimum with the current value. Returns the previous value.
            #[track_caller]
            pub fn fetch_min(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(old.min(val))).unwrap()
            }

            /// Load the atomic value directly without triggering any Shuttle context switches.
            ///
            /// # Safety
            ///
            /// Shuttle does not consider potential concurrent interleavings of this function call,
            /// and so it should be used when those interleavings aren't important (primarily in
            /// debugging scenarios where we might want to just print this atomic's value).
            pub unsafe fn raw_load(&self) -> $int_type {
                self.inner.raw_load()
            }

            #[cfg(test)]
            pub(crate) fn signature(&self) -> crate::sync::ResourceSignature {
                self.inner.signature()
            }
        }
    };
}

atomic_int!(AtomicI8, i8);
atomic_int!(AtomicI16, i16);
atomic_int!(AtomicI32, i32);
atomic_int!(AtomicI64, i64);
atomic_int!(AtomicIsize, isize);
atomic_int!(AtomicU8, u8);
atomic_int!(AtomicU16, u16);
atomic_int!(AtomicU32, u32);
atomic_int!(AtomicU64, u64);
atomic_int!(AtomicUsize, usize);
