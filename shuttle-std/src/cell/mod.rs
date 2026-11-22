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
