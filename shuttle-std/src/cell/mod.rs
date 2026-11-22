//! Shuttle's versions of the types in [`std::cell`].
//!
//! [`UnsafeCell`] gives access to its value through a [`Ptr`] instead of a plain `*mut T`. A `Ptr`
//! reads and writes the value as a raw pointer does, and [`Ptr::as_ref`] and [`Ptr::as_mut`] make
//! a reference to it, as `&*ptr` and `&mut *ptr` do. Each of these accesses is a call into Shuttle,
//! so Shuttle can check it. [`UnsafeCell::with`] and [`UnsafeCell::with_mut`] are loom's API, for
//! code that uses loom's `UnsafeCell`.
//!
//! ```
//! use shuttle_std::cell::UnsafeCell;
//! use shuttle_std::sync::Mutex;
//! use shuttle_std::{sync::Arc, thread};
//!
//! struct Counter {
//!     lock: Mutex<()>,
//!     value: UnsafeCell<u64>,
//! }
//! unsafe impl Sync for Counter {}
//!
//! impl Counter {
//!     fn increment(&self) {
//!         let _guard = self.lock.lock().unwrap();
//!         // Made and used under the lock.
//!         let mut value = unsafe { self.value.get().as_mut() };
//!         *value += 1;
//!     }
//! }
//!
//! shuttle_schedulers::check_random(
//!     || {
//!         let counter = Arc::new(Counter { lock: Mutex::new(()), value: UnsafeCell::new(0) });
//!         let other = Arc::clone(&counter);
//!         let t = thread::spawn(move || other.increment());
//!         counter.increment();
//!         t.join().unwrap();
//!     },
//!     100,
//! );
//! ```

mod unsafe_cell;

pub use unsafe_cell::{Ptr, PtrRef, PtrRefMut, UnsafeCell};

use std::cmp::Ordering;
use std::fmt;

/// A version of [`std::cell::Cell`] that keeps its value in an [`UnsafeCell`], so that each access
/// to the value goes through a [`Ptr`].
///
/// Unlike std's `Cell`, `new` isn't `const`, and the value must be `Sized`, so there is no
/// `from_mut` or `as_slice_of_cells`.
pub struct Cell<T> {
    value: UnsafeCell<T>,
}

impl<T> Cell<T> {
    /// Makes a new cell holding `value`.
    pub fn new(value: T) -> Self {
        Self {
            value: UnsafeCell::new(value),
        }
    }

    /// Sets the value, and drops the old one.
    pub fn set(&self, value: T) {
        drop(self.replace(value));
    }

    /// Swaps the values of two cells.
    pub fn swap(&self, other: &Self) {
        if std::ptr::eq(self, other) {
            return;
        }
        // SAFETY: a `Cell` isn't `Sync` and lends out no reference to its value while it is shared,
        // so nothing else accesses either value now. The two cells are different values, so they
        // don't overlap.
        unsafe {
            let ours = self.value.get().read();
            let theirs = other.value.get().replace(ours);
            self.value.get().write(theirs);
        }
    }

    /// Replaces the value, and returns the old one.
    pub fn replace(&self, value: T) -> T {
        // SAFETY: as in `swap`, nothing else accesses the value now.
        unsafe { self.value.get().replace(value) }
    }

    /// Unwraps the value.
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    /// A raw pointer to the value. Shuttle doesn't see the accesses through it.
    pub fn as_ptr(&self) -> *mut T {
        self.value.get().as_raw()
    }

    /// A mutable reference to the value.
    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }
}

impl<T: Copy> Cell<T> {
    /// A copy of the value.
    pub fn get(&self) -> T {
        // SAFETY: as in `swap`, nothing else accesses the value now.
        unsafe { self.value.get().read() }
    }

    /// Updates the value with `f`.
    pub fn update(&self, f: impl FnOnce(T) -> T) {
        let old = self.get();
        self.set(f(old));
    }
}

impl<T: Default> Cell<T> {
    /// Takes the value, and leaves `T::default()` in its place.
    pub fn take(&self) -> T {
        self.replace(T::default())
    }
}

impl<T: Copy> Clone for Cell<T> {
    fn clone(&self) -> Self {
        Self::new(self.get())
    }
}

impl<T: Default> Default for Cell<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: PartialEq + Copy> PartialEq for Cell<T> {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl<T: Eq + Copy> Eq for Cell<T> {}

impl<T: PartialOrd + Copy> PartialOrd for Cell<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.get().partial_cmp(&other.get())
    }
}

impl<T: Ord + Copy> Ord for Cell<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.get().cmp(&other.get())
    }
}

impl<T> From<T> for Cell<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: Copy + fmt::Debug> fmt::Debug for Cell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cell").field("value", &self.get()).finish()
    }
}
