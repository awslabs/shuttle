//! Atomic types
//!
//! Atomic types provide primitive shared-memory communication between threads, and are the building
//! blocks of other concurrent types.
//!
//! This module defines atomic versions of a select number of primitive types, the same as
//! [`std::sync::atomic`] in the standard library. See that module's documentation for more details.
//!
//! # Warning about relaxed behaviors
//!
//! Shuttle does not faithfully model behaviors of relaxed atomic orderings (those other than
//! [`SeqCst`](Ordering::SeqCst)). Code that uses these orderings may contain bugs that Shuttle is
//! unable to find if the bug requires the relaxed behavior to occur. **Shuttle models *all* atomic
//! operations as if they were using SeqCst ordering.**
//!
//! For example, consider this test that uses a `flag` variable to indicate that data is present in
//! a separate `data` variable:
//! ```
//! # use std::sync::Arc;
//! # use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
//! # use std::thread;
//! let flag = Arc::new(AtomicBool::new(false));
//! let data = Arc::new(AtomicU64::new(0));
//!
//! {
//!     let flag = Arc::clone(&flag);
//!     let data = Arc::clone(&data);
//!     thread::spawn(move|| {
//!         data.store(42, Ordering::Relaxed);
//!         flag.store(true, Ordering::Relaxed);
//!     });
//! }
//!
//! if flag.load(Ordering::Relaxed) {
//!     assert_eq!(data.load(Ordering::Relaxed), 42);
//! }
//! ```
//! This code is incorrect because of the relaxed orderings used for the loads and stores. Some
//! architectures will allow an execution where `flag` is true but the assertion is false. However,
//! Shuttle treats all atomic operations as SeqCst, and this test would be correct if we used SeqCst
//! for all atomic operations, so Shuttle cannot find this bug.
//!
//! If you are writing code that relies on relaxed atomic operations and need to check its
//! correctness, the [Loom] crate provides support for reasoning about Acquire and Release orderings
//! and partial support for Relaxed orderings.
//!
//! To disable the warning printed about this issue, set the `SHUTTLE_SILENCE_ORDERING_WARNING`
//! environment variable to any value, or set the
//! [`silence_atomic_ordering_warning`](crate::Config::silence_atomic_ordering_warning) field of
//! [`Config`](crate::Config) to true.
//!
//! [Loom]: https://crates.io/crates/loom

mod bool;
mod int;
mod ptr;

pub use self::bool::AtomicBool;
pub use int::*;
pub use ptr::AtomicPtr;
pub use std::sync::atomic::Ordering;

use crate::runtime::execution::ExecutionState;
use crate::runtime::thread;
use std::cell::RefCell;

static PRINTED_ORDERING_WARNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[inline]
fn maybe_warn_about_ordering(order: Ordering) {
    use ansi_term::Colour;

    #[allow(clippy::clippy::collapsible_if)]
    if order != Ordering::SeqCst {
        if PRINTED_ORDERING_WARNING
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            if std::env::var("SHUTTLE_SILENCE_ORDERING_WARNING").is_ok() {
                return;
            }

            if ExecutionState::with(|state| state.config.silence_atomic_ordering_warning) {
                return;
            }

            eprintln!(
                "{}: Shuttle only correctly models SeqCst atomics and treats all other Orderings \
                as if they were SeqCst. Bugs caused by weaker orderings like {:?} may be missed. \
                See https://docs.rs/shuttle/*/shuttle/sync/atomic/index.html#warning-about-relaxed-behaviors \
                for details or to disable this warning.",
                Colour::Yellow.normal().paint("WARNING"),
                order
            );
        }
    }
}

/// An atomic fence, like the standard library's [std::sync::atomic::fence].
pub fn fence(order: Ordering) {
    if order == Ordering::Relaxed {
        panic!("there is no such thing as a relaxed fence");
    }

    maybe_warn_about_ordering(order);

    // SeqCst fences are no-ops in our execution model
}

// We can just reuse the standard library's compiler fence, as they have no visible run-time
// behavior and so we need neither insert yieldpoints nor warn about non-SeqCst orderings.
pub use std::sync::atomic::compiler_fence;

/// Base type for atomic implementations. This type handles generating the right interleavings for
/// all atomics. The interesting stuff is in `load`, `store`, `swap`, and `fetch_update`; all other
/// atomic operations are implemented in terms of those four primitives.
#[derive(Debug)]
struct Atomic<T> {
    inner: RefCell<T>,
}

// Safety: Atomic is never actually passed across true threads, only across continuations. The
// RefCell<_> type therefore can't be preempted mid-bookkeeping-operation.
unsafe impl<T: Sync> Sync for Atomic<T> {}

impl<T> Atomic<T> {
    const fn new(v: T) -> Self {
        Self { inner: RefCell::new(v) }
    }
}

impl<T: Copy + Eq> Atomic<T> {
    fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }

    fn into_inner(self) -> T {
        self.inner.into_inner()
    }

    fn load(&self, order: Ordering) -> T {
        maybe_warn_about_ordering(order);

        thread::switch();
        let value = *self.inner.borrow();
        thread::switch();
        value
    }

    fn store(&self, val: T, order: Ordering) {
        maybe_warn_about_ordering(order);

        thread::switch();
        *self.inner.borrow_mut() = val;
        thread::switch();
    }

    fn swap(&self, mut val: T, order: Ordering) -> T {
        maybe_warn_about_ordering(order);

        thread::switch();
        std::mem::swap(&mut *self.inner.borrow_mut(), &mut val);
        thread::switch();
        val
    }

    fn fetch_update<F>(&self, set_order: Ordering, fetch_order: Ordering, mut f: F) -> Result<T, T>
    where
        F: FnMut(T) -> Option<T>,
    {
        maybe_warn_about_ordering(set_order);
        maybe_warn_about_ordering(fetch_order);

        thread::switch();
        let current = *self.inner.borrow();
        let ret = if let Some(new) = f(current) {
            *self.inner.borrow_mut() = new;
            Ok(current)
        } else {
            Err(current)
        };
        thread::switch();
        ret
    }

    unsafe fn raw_load(&self) -> T {
        *self.inner.borrow()
    }
}
