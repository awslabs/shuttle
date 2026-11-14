//! `AtomicCell`, whose operations are atomic steps of a Shuttle execution.

use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::per_execution::{PerExecution, ValueLock};
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::thread;
use std::cell::{RefCell, UnsafeCell};
use std::fmt;
use std::mem::{align_of, size_of};
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::atomic;

/// A thread-safe mutable memory location.
///
/// This type is equivalent to [`Cell`], except it can also be shared among multiple threads.
///
/// [`Cell`]: std::cell::Cell
///
/// # Under Shuttle
///
/// Every operation takes one scheduling point and then runs to completion, with no other task
/// running in between, so it is atomic whatever `T` is, and Shuttle explores the orders in which
/// the tasks reach the cell. The real `AtomicCell` is atomic in the same sense, through an atomic
/// instruction where `T` allows and a lock otherwise.
///
/// A value stored in the cell carries the clock of the task that stored it, which a task that
/// loads it joins, so Shuttle's causality is the same as for one of its own atomics.
///
/// Like the rest of Shuttle's primitives, every operation has to run inside a Shuttle test.
/// [`AtomicCell::new`] does not, so a `static` cell can be created outside one.
///
/// # Differences from `crossbeam_utils::atomic::AtomicCell`
///
/// * [`AtomicCell::into_inner`] is not a `const fn`, because the cell holds a clock whose
///   destructor a `const fn` cannot run. `new` and `as_ptr` are `const`, as crossbeam's are.
/// * `Debug` reads the value, so it has to run inside a Shuttle test.
/// * **A cell that outlives its execution keeps its value**, where every other primitive here
///   starts the next execution over. A reset would have to put back the value the cell was created
///   with, which needs a copy of it, and `AtomicCell<T>` is a cell of any `T`, which may be neither
///   `Copy` nor `Clone`. So a `static AtomicCell` carries what the last execution stored into the
///   next one. The clock does reset: a clock from another execution names tasks that do not exist
///   in this one, which would hide a race. Code that needs the value to reset puts the cell in a
///   `shuttle::lazy_static!`, which does.
///
/// Tests that run at the same time on different threads, as `cargo test` runs a binary's tests, and
/// share a `static AtomicCell` can't race on its value: an operation that finds another test's
/// operation in the middle of the cell panics, saying so, as Shuttle's locks do (see
/// `shuttle-engine`'s `ValueLock`). Their clocks are their own.
#[repr(C)]
pub struct AtomicCell<T> {
    /// The value. Reached only between a scheduling point and the end of the operation, where no
    /// other task of the execution runs, and while `access` keeps other executions out.
    value: UnsafeCell<T>,
    /// Keeps the value to the operations of one execution at a time, which runs them one at a time.
    access: ValueLock,
    /// The clock of the task that stored the value, which a task that reads it joins, in each
    /// execution. `None` until the execution's first operation stores, so that `new` can be a
    /// `const fn`, as Shuttle's own atomics do.
    clock: PerExecution<RefCell<Option<VectorClock>>>,
}

/// The name of the cell in the messages of [`ValueLock::acquire`].
const KIND: &str = "crossbeam AtomicCell";

// Safety: the bounds are crossbeam's. The value is reached only by an operation, between its one
// scheduling point and its end, while it holds `access`: no other task of the execution runs then,
// as Shuttle runs them on one thread and switches between them only at scheduling points, and
// `access` keeps the operations of executions on other threads out. The clock is each execution's
// own (see `PerExecution`).
unsafe impl<T: Send> Send for AtomicCell<T> {}
unsafe impl<T: Send> Sync for AtomicCell<T> {}

impl<T> UnwindSafe for AtomicCell<T> {}
impl<T> RefUnwindSafe for AtomicCell<T> {}

impl<T> AtomicCell<T> {
    /// Creates a new atomic cell initialized with `val`.
    pub const fn new(val: T) -> AtomicCell<T> {
        AtomicCell {
            value: UnsafeCell::new(val),
            access: ValueLock::new(),
            clock: PerExecution::new(),
        }
    }

    /// Consumes the atomic and returns the contained value.
    ///
    /// This is safe because passing `self` by value guarantees that no other threads are
    /// concurrently accessing the atomic data.
    ///
    /// Unlike crossbeam's, this is not a `const fn`: the cell holds a clock whose destructor a
    /// `const fn` cannot run.
    pub fn into_inner(self) -> T {
        // The cell has no `Drop` of its own, so this moves the value out and drops the rest, which
        // runs no user code.
        let Self {
            value,
            access: _,
            clock: _,
        } = self;
        value.into_inner()
    }

    /// Returns `true` if operations on values of this type are lock-free.
    ///
    /// Under Shuttle every operation on a cell is one atomic step, whatever `T` is. This reports
    /// what the real `AtomicCell` would do, so that code which branches on it behaves the same in
    /// both modes: `true` when `T` can be transmuted into one of the primitive atomic types.
    pub const fn is_lock_free() -> bool {
        atomic_is_lock_free::<T>()
    }

    /// Stores `val` into the atomic cell.
    pub fn store(&self, val: T) {
        // The old value is dropped after the operation, outside the borrow of the cell, since its
        // destructor may use Shuttle's primitives.
        drop(self.replace(val, false));
    }

    /// Stores `val` into the atomic cell and returns the previous value.
    pub fn swap(&self, val: T) -> T {
        self.replace(val, true)
    }

    /// Replaces the value, and returns the old one. `reads` says whether the caller observes the
    /// old value, and so whether it joins the clock of the task that stored it.
    fn replace(&self, val: T, reads: bool) -> T {
        thread::switch();
        let _access = self.access();
        if reads {
            self.exhale_clock();
        }
        self.inhale_clock();
        // Safety: the scheduling point above is the operation's only one, so no other task runs
        // until it ends, `_access` keeps other executions out, and nothing else holds a reference to
        // the value. `T`'s destructor does not run here: the old value is returned, to be dropped by
        // the caller, once `_access` has let the value go.
        unsafe { std::mem::replace(&mut *self.value.get(), val) }
    }

    /// Returns a raw pointer to the underlying data in this atomic cell.
    pub const fn as_ptr(&self) -> *mut T {
        self.value.get()
    }

    /// Holds the value until the operation ends, once its scheduling point has passed. An operation
    /// of another execution, on another thread, that holds it now is reported (see [`ValueLock`]).
    fn access(&self) -> Access<'_> {
        self.access.acquire(KIND);
        Access(&self.access)
    }

    /// The clock of the task that stored the value, in the current execution.
    fn clock(&self) -> &RefCell<Option<VectorClock>> {
        self.clock.get(|| RefCell::new(None))
    }

    /// Joins the clock of the task that stored the value, so that a task which reads the value sees
    /// what that task had seen.
    fn exhale_clock(&self) {
        let clock = self.clock().borrow().clone();
        if let Some(clock) = clock {
            ExecutionState::with(|state| state.update_clock(&clock));
        }
    }

    /// Records the storing task's clock with the value.
    fn inhale_clock(&self) {
        let clock = ExecutionState::with(|state| state.increment_clock().clone());
        let mut stored = self.clock().borrow_mut();
        match stored.as_mut() {
            Some(stored) => stored.update(&clock),
            None => *stored = Some(clock),
        }
    }
}

/// An operation's hold on the value of an [`AtomicCell`], which it lets go when it is dropped, at the
/// end of the operation or as a panic unwinds it.
struct Access<'a>(&'a ValueLock);

impl Drop for Access<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

impl<T: Default> AtomicCell<T> {
    /// Takes the value of the atomic cell, leaving `Default::default()` in its place.
    pub fn take(&self) -> T {
        self.swap(Default::default())
    }
}

impl<T: Copy> AtomicCell<T> {
    /// Loads a value from the atomic cell.
    pub fn load(&self) -> T {
        thread::switch();
        let _access = self.access();
        self.exhale_clock();
        // Safety: as in `replace`, and `T: Copy` has no destructor, so reading it runs no user code.
        unsafe { *self.value.get() }
    }
}

impl<T: Copy + Eq> AtomicCell<T> {
    /// If the current value equals `current`, stores `new` into the atomic cell.
    ///
    /// The return value is always the previous value. If it is equal to `current`, then the value
    /// was updated.
    #[deprecated(note = "Use `compare_exchange` instead")]
    pub fn compare_and_swap(&self, current: T, new: T) -> T {
        match self.compare_exchange(current, new) {
            Ok(v) => v,
            Err(v) => v,
        }
    }

    /// If the current value equals `current`, stores `new` into the atomic cell.
    ///
    /// The return value is a result indicating whether the new value was written and containing
    /// the previous value. On success this value is guaranteed to be equal to `current`.
    pub fn compare_exchange(&self, current: T, new: T) -> Result<T, T> {
        self.try_update(|value| (value == current).then_some(new))
    }

    /// Fetches the value, and applies a function to it that returns an optional new value. Returns
    /// a `Result` of `Ok(previous_value)` if the function returned `Some(_)`, else
    /// `Err(previous_value)`.
    ///
    /// Note: This may call the function multiple times if the value has been changed from other
    /// threads in the meantime, as long as the function returns `Some(_)`, but the function will
    /// have been applied only once to the stored value.
    pub fn fetch_update<F>(&self, mut f: F) -> Result<T, T>
    where
        F: FnMut(T) -> Option<T>,
    {
        // As crossbeam's, a loop over `load` and `compare_exchange`, so that the caller's function
        // runs outside the atomic step, where it may use Shuttle's primitives.
        let mut prev = self.load();
        while let Some(next) = f(prev) {
            match self.compare_exchange(prev, next) {
                x @ Ok(_) => return x,
                Err(next_prev) => prev = next_prev,
            }
        }
        Err(prev)
    }

    /// One atomic step that reads the value and stores what `f` returns, if it returns anything.
    /// `f` must not use Shuttle's primitives: it runs inside the step.
    fn try_update(&self, f: impl FnOnce(T) -> Option<T>) -> Result<T, T> {
        thread::switch();
        let _access = self.access();
        self.exhale_clock();
        // Safety: as in `replace`.
        let value = unsafe { &mut *self.value.get() };
        let current = *value;
        match f(current) {
            Some(new) => {
                *value = new;
                self.inhale_clock();
                Ok(current)
            }
            None => Err(current),
        }
    }
}

macro_rules! impl_arithmetic {
    ($t:ty) => {
        impl AtomicCell<$t> {
            /// Increments the current value by `val` and returns the previous value.
            ///
            /// The addition wraps on overflow.
            pub fn fetch_add(&self, val: $t) -> $t {
                self.arithmetic(|value| value.wrapping_add(val))
            }

            /// Decrements the current value by `val` and returns the previous value.
            ///
            /// The subtraction wraps on overflow.
            pub fn fetch_sub(&self, val: $t) -> $t {
                self.arithmetic(|value| value.wrapping_sub(val))
            }

            /// Applies bitwise "and" to the current value and returns the previous value.
            pub fn fetch_and(&self, val: $t) -> $t {
                self.arithmetic(|value| value & val)
            }

            /// Applies bitwise "nand" to the current value and returns the previous value.
            pub fn fetch_nand(&self, val: $t) -> $t {
                self.arithmetic(|value| !(value & val))
            }

            /// Applies bitwise "or" to the current value and returns the previous value.
            pub fn fetch_or(&self, val: $t) -> $t {
                self.arithmetic(|value| value | val)
            }

            /// Applies bitwise "xor" to the current value and returns the previous value.
            pub fn fetch_xor(&self, val: $t) -> $t {
                self.arithmetic(|value| value ^ val)
            }

            /// Compares and sets the maximum of the current value and `val`, and returns the
            /// previous value.
            pub fn fetch_max(&self, val: $t) -> $t {
                self.arithmetic(|value| value.max(val))
            }

            /// Compares and sets the minimum of the current value and `val`, and returns the
            /// previous value.
            pub fn fetch_min(&self, val: $t) -> $t {
                self.arithmetic(|value| value.min(val))
            }

            /// One atomic step that replaces the value with `f` of it, and returns the old one.
            fn arithmetic(&self, f: impl FnOnce($t) -> $t) -> $t {
                match self.try_update(|value| Some(f(value))) {
                    Ok(previous) => previous,
                    Err(_) => unreachable!("the update always stores"),
                }
            }
        }
    };
}

impl_arithmetic!(u8);
impl_arithmetic!(i8);
impl_arithmetic!(u16);
impl_arithmetic!(i16);
impl_arithmetic!(u32);
impl_arithmetic!(i32);
impl_arithmetic!(u64);
impl_arithmetic!(i64);
impl_arithmetic!(u128);
impl_arithmetic!(i128);
impl_arithmetic!(usize);
impl_arithmetic!(isize);

impl AtomicCell<bool> {
    /// Applies logical "and" to the current value and returns the previous value.
    pub fn fetch_and(&self, val: bool) -> bool {
        self.logical(|value| value & val)
    }

    /// Applies logical "nand" to the current value and returns the previous value.
    pub fn fetch_nand(&self, val: bool) -> bool {
        self.logical(|value| !(value & val))
    }

    /// Applies logical "or" to the current value and returns the previous value.
    pub fn fetch_or(&self, val: bool) -> bool {
        self.logical(|value| value | val)
    }

    /// Applies logical "xor" to the current value and returns the previous value.
    pub fn fetch_xor(&self, val: bool) -> bool {
        self.logical(|value| value ^ val)
    }

    fn logical(&self, f: impl FnOnce(bool) -> bool) -> bool {
        match self.try_update(|value| Some(f(value))) {
            Ok(previous) => previous,
            Err(_) => unreachable!("the update always stores"),
        }
    }
}

impl<T: Default> Default for AtomicCell<T> {
    fn default() -> AtomicCell<T> {
        AtomicCell::new(T::default())
    }
}

impl<T> From<T> for AtomicCell<T> {
    #[inline]
    fn from(val: T) -> AtomicCell<T> {
        AtomicCell::new(val)
    }
}

impl<T: Copy + fmt::Debug> fmt::Debug for AtomicCell<T> {
    /// Reads the value, so it has to run inside a Shuttle test.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtomicCell").field("value", &self.load()).finish()
    }
}

/// Returns `true` if values of type `A` can be transmuted into values of type `B`.
const fn can_transmute<A, B>() -> bool {
    // Sizes must be equal, but alignment of `A` must be greater or equal than that of `B`.
    (size_of::<A>() == size_of::<B>()) & (align_of::<A>() >= align_of::<B>())
}

/// Whether the real `AtomicCell<T>` would use an atomic instruction rather than a lock, which is
/// what [`AtomicCell::is_lock_free`] reports. The list of atomic types is the one crossbeam's
/// `atomic!` macro tries.
const fn atomic_is_lock_free<T>() -> bool {
    #[cfg(target_has_atomic = "64")]
    let sixty_four = can_transmute::<T, atomic::AtomicU64>();
    #[cfg(not(target_has_atomic = "64"))]
    let sixty_four = false;

    can_transmute::<T, ()>()
        || can_transmute::<T, atomic::AtomicU8>()
        || can_transmute::<T, atomic::AtomicU16>()
        || can_transmute::<T, atomic::AtomicU32>()
        || sixty_four
}
