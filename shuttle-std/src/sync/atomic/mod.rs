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
//! for all atomic operations, so Shuttle cannot find this bug, unless the test turns on the
//! weak-memory emulation described below.
//!
//! If you are writing code that relies on relaxed atomic operations and need to check its
//! correctness, the [Loom] crate provides support for reasoning about Acquire and Release orderings
//! and partial support for Relaxed orderings.
//!
//! To disable the warning printed about this issue, set the `SHUTTLE_SILENCE_WARNINGS` environment
//! variable to any value, or set the [`silence_warnings`](shuttle_engine::Config::silence_warnings) field of
//! [`Config`](shuttle_engine::Config) to true.
//!
//! [Loom]: https://crates.io/crates/loom
//!
//! # Weak-memory emulation
//!
//! With the `weak-memory` feature, a test can set
//! [`weak_memory`](shuttle_engine::Config::weak_memory) in its [`Config`](shuttle_engine::Config)
//! to make atomic loads return older stores, as orderings weaker than SeqCst allow. The emulation is
//! a port of [Miri's](https://github.com/rust-lang/miri). Each atomic keeps its last 128 stores, and
//! a load reads one of those that coherence, happens-before and the SeqCst rules still allow. The
//! store is picked with the scheduler's random numbers, so a failing schedule replays. The test
//! above then finds the bug:
//! ```no_run
//! # use shuttle_std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
//! # use shuttle_std::sync::Arc;
//! # use shuttle_std::thread;
//! # use shuttle_engine::{Config, Runner};
//! # use shuttle_schedulers::RandomScheduler;
//! let mut config = Config::new();
//! config.weak_memory = true;
//! Runner::new(RandomScheduler::new(1000), config).run(|| {
//!     let flag = Arc::new(AtomicBool::new(false));
//!     let data = Arc::new(AtomicU64::new(0));
//!     {
//!         let flag = Arc::clone(&flag);
//!         let data = Arc::clone(&data);
//!         thread::spawn(move || {
//!             data.store(42, Ordering::Relaxed);
//!             flag.store(true, Ordering::Relaxed);
//!         });
//!     }
//!     if flag.load(Ordering::Relaxed) {
//!         assert_eq!(data.load(Ordering::Relaxed), 42); // fails in some executions
//!     }
//! });
//! ```
//!
//! What the emulation does not do:
//! - It produces only some of the outcomes that the memory model allows. A load reads only stores
//!   that the schedule has already run, so outcomes that need a load to read a later store, such
//!   as "load buffering", never show up. SeqCst fences synchronize with each other, which is
//!   stronger than C++20 requires.
//! - It applies only to Shuttle's atomics. Memory that isn't atomic always holds the latest write,
//!   so a bug in which a too-weak ordering publishes non-atomic data shows up only if the data is
//!   read through an atomic.
//! - The DFS scheduler doesn't explore which store a load reads. It panics when a load has a
//!   choice, unless it was created with `allow_random_data`. Then every execution gets the same
//!   random numbers, so DFS tries only some of the choices.
//! - `compare_exchange_weak` never fails spuriously.

mod bool;
mod int;
mod ptr;
#[cfg(feature = "weak-memory")]
mod weak_memory;

pub use self::bool::AtomicBool;
pub use int::*;
pub use ptr::AtomicPtr;
pub use std::sync::atomic::Ordering;

use crate::sync::{ResourceSignature, ResourceType};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::thread;
use shuttle_engine::silence_warnings;
use std::cell::RefCell;
use std::panic::RefUnwindSafe;

static PRINTED_ORDERING_WARNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[inline]
fn maybe_warn_about_ordering(order: Ordering) {
    use owo_colors::OwoColorize;

    #[allow(clippy::collapsible_if)]
    if order != Ordering::SeqCst {
        if PRINTED_ORDERING_WARNING
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            if silence_warnings() {
                return;
            }

            if ExecutionState::with(|state| state.config.silence_warnings || state.config.weak_memory) {
                return;
            }

            eprintln!(
                "{}: Shuttle only correctly models SeqCst atomics and treats all other Orderings \
                as if they were SeqCst. Bugs caused by weaker orderings like {:?} may be missed. \
                See https://docs.rs/shuttle/*/shuttle/sync/atomic/index.html#warning-about-relaxed-behaviors \
                for details or to disable this warning.",
                "WARNING".yellow(),
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

    #[cfg(feature = "weak-memory")]
    if weak_memory::enabled() {
        thread::switch();
        weak_memory::fence(order);
    }

    // Without weak-memory emulation, every access is SeqCst, so fences are no-ops
}

// We can just reuse the standard library's compiler fence, as they have no visible run-time
// behavior and so we need neither insert yieldpoints nor warn about non-SeqCst orderings.
pub use std::sync::atomic::compiler_fence;

/// Base type for atomic implementations. This type handles generating the right interleavings for
/// all atomics. The interesting stuff is in `load`, `store`, `swap`, and `try_update`; all other
/// atomic operations are implemented in terms of those four primitives.
#[derive(Debug)]
struct Atomic<T> {
    inner: RefCell<T>,
    clock: RefCell<Option<VectorClock>>, // wrapped in option to support the const new()
    // the stores that loads can still read, when the execution emulates weak memory. `inner` holds
    // the newest one's value either way.
    #[cfg(feature = "weak-memory")]
    stores: RefCell<weak_memory::StoreBuffer<T>>,
    #[allow(unused)]
    signature: ResourceSignature,
}

// Safety: Atomic is never actually passed across true threads, only across continuations. The
// RefCell<_> type therefore can't be preempted mid-bookkeeping-operation.
unsafe impl<T: Sync> Sync for Atomic<T> {}
impl<T: RefUnwindSafe> RefUnwindSafe for Atomic<T> {}

impl<T> Atomic<T> {
    #[track_caller]
    const fn new(v: T) -> Self {
        // Since this is a `const fn`, the clock associated with this Atomic is assigned a const value of None
        // (which represents all zeros).  At the time of creation of the atomic, however, we have more causal
        // knowledge (the value of the current thread's vector clock).  However, it should be safe to initialize
        // the Atomic's clock to all-zeros because the only way for another thread to access this Atomic is for
        // a reference to it to be passed to it via some other synchronization mechanism, which will carry the
        // knowledge about the owning thread's clock.
        // TODO Check that the argument above is sound
        Self {
            inner: RefCell::new(v),
            clock: RefCell::new(None),
            #[cfg(feature = "weak-memory")]
            stores: RefCell::new(weak_memory::StoreBuffer::new()),
            signature: ResourceSignature::new_const(ResourceType::Atomic),
        }
    }
}

impl<T: Copy + Eq> Atomic<T> {
    fn get_mut(&mut self) -> &mut T {
        self.exhale_clock();
        // Every earlier access happens before an exclusive one, so later loads can only read the
        // value it leaves.
        #[cfg(feature = "weak-memory")]
        self.stores.get_mut().clear();
        self.inner.get_mut()
    }

    fn into_inner(self) -> T {
        self.exhale_clock();
        self.inner.into_inner()
    }

    fn load(&self, order: Ordering) -> T {
        maybe_warn_about_ordering(order);

        thread::switch();
        #[cfg(feature = "weak-memory")]
        if weak_memory::enabled() {
            return weak_memory::load(&self.stores, &self.inner, order);
        }
        self.exhale_clock();
        let value = *self.inner.borrow();
        value
    }

    fn store(&self, val: T, order: Ordering) {
        maybe_warn_about_ordering(order);

        thread::switch();
        #[cfg(feature = "weak-memory")]
        if weak_memory::enabled() {
            return weak_memory::store(&self.stores, &self.inner, val, order);
        }
        self.inhale_clock();
        *self.inner.borrow_mut() = val;
    }

    fn swap(&self, mut val: T, order: Ordering) -> T {
        maybe_warn_about_ordering(order);

        // swap behaves like { let x = load() ; store(val) ; x }
        thread::switch();
        #[cfg(feature = "weak-memory")]
        if weak_memory::enabled() {
            // The closure always returns a value, so this is always `Ok`.
            return weak_memory::rmw(&self.stores, &self.inner, order, order, |_| Some(val)).unwrap_or_else(|old| old);
        }
        self.exhale_clock(); // for the load
        self.inhale_clock(); // for the store
        std::mem::swap(&mut *self.inner.borrow_mut(), &mut val);
        val
    }

    fn try_update<F>(&self, set_order: Ordering, fetch_order: Ordering, mut f: F) -> Result<T, T>
    where
        F: FnMut(T) -> Option<T>,
    {
        maybe_warn_about_ordering(set_order);
        maybe_warn_about_ordering(fetch_order);

        // try_update behaves like (ignoring error): { let x = load() ; store(f(x)); x }
        // in the error case, there is no store, so the register does not inherit the clock of the caller
        //
        // NOTE: this is implemented as a single atomic step, so `f` must be pure. We can guarantee
        // that internally, but not for user-provided callbacks. The public `try_update` and
        // `update` methods run user callbacks, so they loop over `load` and `compare_exchange_weak`
        // instead, just like `std`.
        thread::switch();
        #[cfg(feature = "weak-memory")]
        if weak_memory::enabled() {
            return weak_memory::rmw(&self.stores, &self.inner, set_order, fetch_order, f);
        }
        self.exhale_clock(); // for the load()
        let current = *self.inner.borrow();
        let ret = if let Some(new) = f(current) {
            *self.inner.borrow_mut() = new;
            self.inhale_clock(); // for the store()
            Ok(current)
        } else {
            Err(current)
        };
        ret
    }

    unsafe fn raw_load(&self) -> T {
        *self.inner.borrow()
    }

    fn init_clock(&self) {
        self.clock.borrow_mut().get_or_insert(VectorClock::new());
    }

    // Increment the clock for the current thread, and update the Atomic's clock with it
    // The Atomic (self) "inhales" the clock from the thread
    fn inhale_clock(&self) {
        self.init_clock();
        ExecutionState::with(|s| {
            let clock = s.increment_clock();
            let mut self_clock = self.clock.borrow_mut();
            self_clock.as_mut().unwrap().update(clock);
        });
    }

    // Increment the clock for the current thread, and update with the Atomic's current clock
    // The Atomic (self) "exhales" its clock to the thread
    fn exhale_clock(&self) {
        self.init_clock();
        ExecutionState::with(|s| {
            let self_clock = self.clock.borrow();
            s.update_clock(self_clock.as_ref().unwrap());
        });
    }

    #[cfg(test)]
    fn signature(&self) -> ResourceSignature {
        self.signature.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::sync::atomic::*;

    #[test]
    fn unique_resource_signatures() {
        let atomic_i8 = AtomicI8::new(0);
        let atomic_i16 = AtomicI16::new(0);
        let atomic_i32 = AtomicI32::new(0);
        let atomic_i64 = AtomicI64::new(0);
        let atomic_i128 = AtomicI128::new(0);
        let atomic_isize = AtomicIsize::new(0);
        let atomic_u8 = AtomicU8::new(0);
        let atomic_u16 = AtomicU16::new(0);
        let atomic_u32 = AtomicU32::new(0);
        let atomic_u64 = AtomicU64::new(0);
        let atomic_u128 = AtomicU128::new(0);
        let atomic_usize = AtomicUsize::new(0);
        let atomic_bool = AtomicBool::new(false);
        let atomic_ptr = AtomicPtr::new(std::ptr::null_mut::<i32>());

        // All atomics should have unique signatures
        let signatures = HashSet::from([
            atomic_i8.signature(),
            atomic_i16.signature(),
            atomic_i32.signature(),
            atomic_i64.signature(),
            atomic_i128.signature(),
            atomic_isize.signature(),
            atomic_u8.signature(),
            atomic_u16.signature(),
            atomic_u32.signature(),
            atomic_u64.signature(),
            atomic_u128.signature(),
            atomic_usize.signature(),
            atomic_bool.signature(),
            atomic_ptr.signature(),
        ]);

        // Check all signatures are unique
        assert_eq!(signatures.len(), 14);
    }

    #[test]
    fn atomic_signatures_consistent_across_shuttle_iterations() {
        use std::sync::{Arc, Mutex};

        let all_signatures = Arc::new(Mutex::new(HashSet::new()));
        let all_signatures_clone = all_signatures.clone();

        shuttle_schedulers::check_random(
            move || {
                let atomic1 = AtomicBool::new(false);
                let atomic2 = AtomicBool::new(true);

                all_signatures_clone
                    .lock()
                    .unwrap()
                    .insert((atomic1.load(Ordering::SeqCst), atomic1.signature()));
                all_signatures_clone
                    .lock()
                    .unwrap()
                    .insert((atomic2.load(Ordering::SeqCst), atomic2.signature()));
            },
            10,
        );

        // Should have exactly 2 unique (signatures X values) across all iterations
        assert_eq!(all_signatures.lock().unwrap().len(), 2);
    }
}
