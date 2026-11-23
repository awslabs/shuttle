//! Shuttle's versions of the types in [`std::cell`].
//!
//! [`UnsafeCell`] gives access to its value through a [`Ptr`] instead of a plain `*mut T`. A `Ptr`
//! reads and writes the value as a raw pointer does, and [`Ptr::as_ref`] and [`Ptr::as_mut`] make
//! a reference to it, as `&*ptr` and `&mut *ptr` do. Each of these accesses is a call into Shuttle,
//! so Shuttle can check it. [`UnsafeCell::with`] and [`UnsafeCell::with_mut`] are loom's API, for
//! code that uses loom's `UnsafeCell`.
//!
//! # Checks
//!
//! With the `checked-cell` feature, Shuttle checks the memory in an `UnsafeCell` for Tree Borrows
//! violations and data races. Each reference made from the cell gets a tag, and Shuttle applies the
//! rules of Tree Borrows, ported from [Miri](https://github.com/rust-lang/miri), to every access.
//! It also checks every access for a data race with Shuttle's vector clocks, as Miri's race detector
//! does. The first violation panics with a report, so the test fails with a schedule that replays
//! it. A violation while a panic unwinds can't panic, because that would abort the process. If that
//! panic fails the test, Shuttle prints the report with it. If the code catches the panic, the
//! execution fails with the report when it ends, and so it does after a violation whose own panic
//! the code catches.
//!
//! Every atomic operation orders memory as SeqCst does, so a race that a too-weak ordering causes
//! isn't reported.
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
//!         // Made and used under the lock. Making it before the lock would be UB under Tree
//!         // Borrows, once another thread writes in between, and Shuttle reports that.
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
//!
//! Shuttle checks each read and write through a `Ptr`, each dereference of a [`PtrRef`] or
//! [`PtrRefMut`] (a `DerefMut` counts as a write), and the read that making a reference does.
//! [`Ptr::project`] makes a pointer to a field, as `&raw mut (*p).field` does. `with` and
//! `with_mut` each count as one read or write through the cell's raw pointer, before the closure
//! runs.
//!
//! What Shuttle doesn't see:
//! - Accesses through a plain reference or raw pointer that the code takes out of a `PtrRef`,
//!   `PtrRefMut` or `Ptr`, as with `&mut *guard`, [`Ptr::as_raw`] or the pointer that `with`
//!   passes. Only the access that took it out is checked.
//! - Which part of the value a guard's code uses. A dereference of a guard reads all of `T`, and
//!   a `DerefMut` writes all of it, even if the code uses one field or writes nothing. So Shuttle
//!   reports some code that Miri accepts, such as a guard to a struct that reads one field after a
//!   write through the cell to another field. For an access to one field, use
//!   [`PtrRefMut::as_ptr`] or [`PtrRef::as_ptr`] with [`Ptr::project`].
//! - A hand-off through something that doesn't keep Shuttle's vector clocks. Shuttle can then
//!   report a data race that isn't one. Examples are std's `Arc`, which `shuttle::sync::Arc` is
//!   (Shuttle doesn't see that the last drop of an `Arc` comes after the others), the channels of
//!   the `futures` crate, shuttle-tokio's `oneshot` and `Notify`, the std atomic that the fast path
//!   of shuttle-tokio's `OnceCell::get` reads, and `thread::park` and `Thread::unpark`, which keep no
//!   clocks. [`UnsafeCell::get_mut`] and
//!   [`UnsafeCell::into_inner`] aren't checked for races, so a `Drop` impl should reach the cell
//!   through `get_mut`. Dropping the cell isn't checked for a race either. A use after the drop is
//!   still reported.
//! - Races while the execution is torn down. Teardown drops the futures of unfinished tasks, and
//!   nothing orders those drops after the other accesses.
//! - A checked cell in a static, which two executions use at once, as parallel tests do. The cell
//!   holds one registration, for the execution that used it last, so the checks miss violations.
//!   Run tests that share a static checked cell one at a time.
//! - Function calls, so no tag is protected. Miri's protectors make some code UB at once that is
//!   UB here only when a disabled reference is used again.
//! - Interior mutability inside the cell. A `&T` is `Frozen` even if `T` has an atomic in it.
//!   Accesses to such an atomic don't go through the cell, so they aren't checked here.
//! - Reads of uninitialized memory, and memory that isn't in a checked cell.

mod checks;
mod unsafe_cell;

pub use unsafe_cell::{Ptr, PtrRef, PtrRefMut, UnsafeCell};
