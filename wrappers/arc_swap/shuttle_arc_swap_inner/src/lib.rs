//! This crate contains Shuttle's internal implementation of the [`arc-swap`] crate, version 1.9.
//! Do not depend on this crate directly. Use the `shuttle-arc-swap` crate, which conditionally
//! exposes this implementation with the `shuttle` feature or the original crate without it.
//!
//! [`arc-swap`]: <https://crates.io/crates/arc-swap/1.9.3>
//!
//! # Under Shuttle
//!
//! arc-swap stores an `Arc` in an [`AtomicPtr`](std::sync::atomic::AtomicPtr) and protects a loaded
//! value with per-thread "debt" slots, so that a read needs no reference count and a write waits
//! for the readers that still owe one. None of that is a scheduling point, so Shuttle never runs
//! another task around a `load` or a `store` and misses the bugs that need such an interleaving;
//! the debt slots are also thread-locals of the one OS thread an execution runs on, which every
//! task would share.
//!
//! This implementation keeps the same pointer, but:
//!
//! - **Every operation is one scheduling point and one step.** [`load`](ArcSwapAny::load),
//!   [`load_full`](ArcSwapAny::load_full), [`store`](ArcSwapAny::store),
//!   [`swap`](ArcSwapAny::swap) and [`compare_and_swap`](ArcSwapAny::compare_and_swap) take a
//!   scheduling point and then read or replace the pointer without another one, so each is atomic
//!   with respect to the other tasks and the operations are linearizable: they happen in the order
//!   the schedule puts them in. [`rcu`](ArcSwapAny::rcu) is arc-swap's loop over `load` and
//!   `compare_and_swap`, so it takes their points, two per attempt.
//! - **A loaded value is a clone of the smart pointer.** [`Guard`] holds an `Arc` (or whatever `T`
//!   is), so the value it points at stays alive for as long as the guard does, whatever the other
//!   tasks store, and nothing is freed while a guard can reach it: no use after free. A `store`,
//!   `swap` or successful `compare_and_swap` gives up the storage's own reference count at the
//!   moment it replaces the pointer, so the value is freed when the last guard and the last clone
//!   of it are dropped. There is no deferred reclamation, so a write never waits for a reader.
//! - **No thread-locals and no debt.** `load` costs a reference count instead, which is the one
//!   visible difference: while a guard from `load` is alive, `Arc::strong_count` of the value is
//!   one higher than it would be with arc-swap, whose `load` leaves the count alone. Code that
//!   asserts on `strong_count` while holding a guard therefore sees a different number.
//! - **Causality is tracked**, as it is for Shuttle's atomics: a task that loads a value is
//!   causally after the store that put it there, so Shuttle's deadlock and race reporting is
//!   accurate.
//! - **A storage keeps its initial value.** Every execution starts from the value the storage was
//!   constructed with, so the storage holds a reference count for it until the storage is dropped,
//!   where arc-swap gives that count up as soon as the value is replaced. The initial value of a
//!   storage that outlives the execution (a `static`, or one a test's closure captures) is
//!   therefore freed when the storage is, and not before.
//! - **Every other value is freed at the end of the execution that stored it**, at the latest:
//!   the execution's state gives up its count when Shuttle tears the execution down, as
//!   `shuttle::lazy_static!` does. Replacing a value frees it as soon as the last `Guard` and the
//!   last clone of it are gone, as in arc-swap.
//! - **The value of a `static` resets per execution.** The stored pointer lives in the running
//!   execution's storage rather than in the `ArcSwapAny`, so a `static` storage — whether built by
//!   [`ArcSwapOption::const_empty`] or reached through a `lazy_static!`, `Lazy` or `OnceLock` —
//!   holds the value it was constructed with at the start of every execution, and two Shuttle tests
//!   that run at the same time and share one `static` do not see each other's values. A storage
//!   created in an earlier execution therefore does not carry that execution's value into the next
//!   one, which would stop a failing schedule from replaying.
//!
//! Everything else is arc-swap's: the [`access`] and [`cache`] modules, the [`RefCnt`] trait and
//! its implementations, [`AsRaw`], the sealed [`strategy`] traits with
//! [`DefaultStrategy`]/[`IndependentStrategy`], the `serde` and `weak` features, and the
//! `Debug`/`Display`/`Default`/`From` implementations.

#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod access;
mod as_raw;
pub mod cache;
pub mod docs;
mod ref_cnt;
#[cfg(feature = "serde")]
mod serde;
pub mod strategy;

use crate::access::{Access, Map};
pub use crate::as_raw::AsRaw;
pub use crate::cache::Cache;
pub use crate::ref_cnt::RefCnt;
use crate::strategy::sealed::Protected;
use crate::strategy::{CaS, Strategy};
pub use crate::strategy::{DefaultStrategy, IndependentStrategy};

use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::storage::StorageKey;
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::thread;
use std::borrow::Borrow;
use std::cell::{Cell, RefCell};
use std::fmt::{Debug, Display, Formatter, Result as FmtResult};
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr;
use std::sync::Arc;
use std::sync::LazyLock as StdLazyLock;
use std::sync::atomic::{AtomicPtr as StdAtomicPtr, AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};

/// Source of the identifiers that key a storage's per-execution state.
static NEXT_ID: StdAtomicUsize = StdAtomicUsize::new(1);

/// The second half of the key of every state this crate stores. It tells these slots apart from
/// Shuttle's own, whose identifiers come from other counters (`Once` uses 0x2, `OnceLock` 0x4) or
/// are addresses (`lazy_static` uses 0x3).
const STORAGE_TAG: usize = 0x6172_6373;

fn next_id() -> usize {
    let id = NEXT_ID.fetch_add(1, StdOrdering::Relaxed);
    assert_ne!(id, 0, "id overflow");
    id
}

/// A temporary storage of the pointer.
///
/// This guard object is returned from most loading methods (with the notable exception of
/// [`load_full`](ArcSwapAny::load_full)). It dereferences to the smart pointer loaded, so most
/// operations are to be done using that.
///
/// Under Shuttle the guard holds a clone of the smart pointer, so the value stays alive for as long
/// as the guard does. See the crate documentation.
pub struct Guard<T: RefCnt, S: Strategy<T> = DefaultStrategy> {
    inner: S::Protected,
}

impl<T: RefCnt, S: Strategy<T>> Guard<T, S> {
    /// Converts it into the held value.
    ///
    /// This, on occasion, may be a tiny bit faster than cloning the Arc or whatever is being held
    /// inside.
    // Associated function on purpose, because of deref
    #[allow(clippy::wrong_self_convention)]
    #[inline]
    pub fn into_inner(lease: Self) -> T {
        lease.inner.into_inner()
    }

    /// Create a guard for a given value `inner`.
    ///
    /// This can be useful on occasion to pass a specific object to code that expects or wants to
    /// store a Guard.
    pub fn from_inner(inner: T) -> Self {
        Guard {
            inner: S::Protected::from_inner(inner),
        }
    }
}

impl<T: RefCnt, S: Strategy<T>> Deref for Guard<T, S> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        self.inner.borrow()
    }
}

impl<T: RefCnt, S: Strategy<T>> From<T> for Guard<T, S> {
    fn from(inner: T) -> Self {
        Self::from_inner(inner)
    }
}

impl<T: Default + RefCnt, S: Strategy<T>> Default for Guard<T, S> {
    fn default() -> Self {
        Self::from(T::default())
    }
}

impl<T: Debug + RefCnt, S: Strategy<T>> Debug for Guard<T, S> {
    fn fmt(&self, formatter: &mut Formatter) -> FmtResult {
        self.deref().fmt(formatter)
    }
}

impl<T: Display + RefCnt, S: Strategy<T>> Display for Guard<T, S> {
    fn fmt(&self, formatter: &mut Formatter) -> FmtResult {
        self.deref().fmt(formatter)
    }
}

/// Comparison of two pointer-like things.
#[allow(clippy::needless_pass_by_value)]
fn ptr_eq<Base, A, B>(a: A, b: B) -> bool
where
    A: AsRaw<Base>,
    B: AsRaw<Base>,
{
    let a = a.as_raw();
    let b = b.as_raw();
    ptr::eq(a, b)
}

/// Takes the reference count `ptr` owns, clones it (so the value gains one), and returns the clone,
/// leaving the original count where it was.
///
/// # Safety
///
/// `ptr` must be a pointer a `T` owns a reference count for, which stays valid for the call.
unsafe fn clone_from_ptr<T: RefCnt>(ptr: *mut T::Base) -> T {
    // Safety: the caller's. `ManuallyDrop` keeps the count `ptr` owns where it is.
    let stored = std::mem::ManuallyDrop::new(unsafe { T::from_ptr(ptr) });
    T::clone(&stored)
}

/// An atomic storage for a reference counted smart pointer like [`Arc`] or `Option<Arc>`.
///
/// This is a storage where a smart pointer may live. It can be read and written atomically from
/// several threads, but doesn't act like a pointer itself.
///
/// One can be created [`from`](From::from) an [`Arc`]. To get the pointer back, use the
/// [`load`](ArcSwapAny::load).
///
/// # Note
///
/// This is the common generic implementation. This allows sharing the same code for storing both
/// `Arc` and `Option<Arc>` (and possibly other similar types).
///
/// In your code, you most probably want to interact with it through the [`ArcSwap`] and
/// [`ArcSwapOption`] aliases. However, the methods they share are described here and are
/// applicable to both of them.
///
/// # Type parameters
///
/// * `T`: The smart pointer to be kept inside. This crate provides implementation for `Arc<_>` and
///   `Option<Arc<_>>` (`Rc` too, but that one is not practically useful). But third party could
///   provide implementations of the [`RefCnt`] trait and plug in others.
/// * `S`: The [strategy] used to protect the data inside. Under Shuttle there is
///   one, which [`DefaultStrategy`] names.
///
/// # Examples
///
/// ```
/// # use std::sync::Arc;
/// # use shuttle_arc_swap_inner::ArcSwap;
/// # shuttle::check_dfs(|| {
/// let arc = Arc::new(42);
/// let arc_swap = ArcSwap::from(arc);
/// assert_eq!(42, **arc_swap.load());
/// // It can be read multiple times
/// assert_eq!(42, **arc_swap.load());
///
/// // Put a new one in there
/// let new_arc = Arc::new(0);
/// assert_eq!(42, *arc_swap.swap(new_arc));
/// assert_eq!(0, **arc_swap.load());
/// # }, None);
/// ```
pub struct ArcSwapAny<T: RefCnt, S: Strategy<T> = DefaultStrategy> {
    /// The pointer the storage was constructed with, which it owns one reference count for until
    /// it is dropped. Every execution starts from this pointer (see [`ArcSwapAny::state`]), so a
    /// storage made outside of a Shuttle test and used by several executions holds the value it was
    /// made with at the start of each of them. Null when the storage was made empty, which only an
    /// `Option` or a `Weak` can be.
    ///
    /// An atomic because a `static` made by [`ArcSwapOption::const_empty`] is shared by every
    /// execution, and several Shuttle tests can run at the same time, on different OS threads. It
    /// only ever changes in [`ArcSwapAny::into_inner`] and in `Drop`, which have the storage to
    /// themselves.
    initial: StdAtomicPtr<T::Base>,

    /// Identifies this storage's state in the running execution. Assigned on first use, so that
    /// `const_empty` stays `const`.
    id: StdLazyLock<usize>,

    /// We are basically an Arc in disguise. Inherit parameters from Arc by pretending to contain
    /// it.
    _phantom_arc: PhantomData<T>,

    /// Strategy to protect the data.
    #[allow(unused)]
    strategy: S,
}

/// What a storage holds in one execution: the pointer, and the clocks of the stores that put values
/// there. It lives in the execution's storage, so a `static` storage starts every execution with
/// the value it was constructed with, and two tests that run at the same time and share a `static`
/// each have their own.
struct State {
    /// The stored pointer, as `*mut ()`: the state does not name the value's type, which does not
    /// have to be `'static` as a stored value would. The storage that owns the state casts it back.
    ptr: Cell<*mut ()>,

    /// Whether the state owns a reference count for `ptr`, which it does for every value stored in
    /// this execution. It does not for the storage's initial value, which the storage itself owns a
    /// count for, so that every execution can start from it.
    owns: Cell<bool>,

    /// The join of the clocks of the tasks that stored a value, so that a task that loads one is
    /// causally after the store it reads. As for Shuttle's atomics, every operation is treated as
    /// `SeqCst`.
    clock: RefCell<VectorClock>,

    /// Gives up the reference count `ptr` owns, for the type the storage holds. Execution teardown
    /// calls it, so the value a storage holds at the end of an execution is dropped then, as
    /// `shuttle::lazy_static!`'s is.
    dec: unsafe fn(*mut ()),
}

impl State {
    /// Takes causality from every store so far: the reading half of an operation.
    fn observe(&self) {
        let clock = self.clock.borrow().clone();
        ExecutionState::with(|execution| execution.update_clock(&clock));
    }

    /// Records a store by the current task, for the tasks that load it later.
    fn publish(&self) {
        let clock = ExecutionState::with(|execution| execution.increment_clock().clone());
        self.clock.borrow_mut().update(&clock);
    }
}

impl Drop for State {
    fn drop(&mut self) {
        let ptr = self.ptr.replace(ptr::null_mut());
        if self.owns.replace(false) && !ptr.is_null() {
            // Safety: the state owns the count its pointer stands for, and nothing holds the
            // pointer afterwards: a `Guard` holds a clone of its own.
            unsafe { (self.dec)(ptr) };
        }
    }
}

/// [`State::dec`] for a storage of `T`.
///
/// # Safety
///
/// `ptr` must be non-null and own a reference count of a `T`.
unsafe fn dec_erased<T: RefCnt>(ptr: *mut ()) {
    // Safety: the caller's.
    unsafe { T::dec(ptr as *mut T::Base) };
}

impl<T: RefCnt, S: Default + Strategy<T>> From<T> for ArcSwapAny<T, S> {
    fn from(val: T) -> Self {
        Self::with_strategy(val, S::default())
    }
}

impl<T: RefCnt, S: Strategy<T>> Drop for ArcSwapAny<T, S> {
    fn drop(&mut self) {
        // The count the storage still holds, if no execution has taken it. The one an execution
        // took is given up when that execution's state is dropped, at the end of the execution.
        let initial = self.initial.swap(ptr::null_mut(), StdOrdering::AcqRel);
        if !initial.is_null() {
            // Safety: we are getting rid of the one stored reference count.
            unsafe { T::dec(initial) };
        }
    }
}

impl<T, S: Strategy<T>> Debug for ArcSwapAny<T, S>
where
    T: Debug + RefCnt,
{
    fn fmt(&self, formatter: &mut Formatter) -> FmtResult {
        formatter.debug_tuple("ArcSwapAny").field(&self.load()).finish()
    }
}

impl<T, S: Strategy<T>> Display for ArcSwapAny<T, S>
where
    T: Display + RefCnt,
{
    fn fmt(&self, formatter: &mut Formatter) -> FmtResult {
        self.load().fmt(formatter)
    }
}

impl<T: RefCnt + Default, S: Default + Strategy<T>> Default for ArcSwapAny<T, S> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: RefCnt, S: Strategy<T>> ArcSwapAny<T, S> {
    /// Constructs a new storage.
    pub fn new(val: T) -> Self
    where
        S: Default,
    {
        Self::from(val)
    }

    /// Constructs a new storage while customizing the protection strategy.
    pub fn with_strategy(val: T, strategy: S) -> Self {
        // The pointer owns the reference count `val` had, until an execution takes it.
        Self {
            initial: StdAtomicPtr::new(T::into_ptr(val)),
            id: StdLazyLock::new(next_id),
            _phantom_arc: PhantomData,
            strategy,
        }
    }

    /// This storage's state in the running execution, created at its first use there.
    ///
    /// Panics outside of a Shuttle execution, as Shuttle's primitives do.
    ///
    /// The state lives in the execution's storage, which keeps it at the same address until the
    /// execution ends, so the reference stays valid for as long as the operation that asked for it
    /// runs, including across scheduling points.
    fn state(&self) -> &State {
        let key = StorageKey(*self.id, STORAGE_TAG);
        let state = ExecutionState::with(|execution| {
            if execution.get_storage::<_, State>(key).is_none() {
                // Every execution starts from the storage's initial value, which the storage keeps
                // a reference count for, so the state only borrows it.
                let initial = self.initial.load(StdOrdering::Acquire);
                execution.init_storage(
                    key,
                    State {
                        ptr: Cell::new(initial as *mut ()),
                        owns: Cell::new(false),
                        clock: RefCell::new(VectorClock::new()),
                        dec: dec_erased::<T>,
                    },
                );
            }
            execution.get_storage::<_, State>(key).expect("just initialized") as *const State
        });
        // Safety: see above.
        unsafe { &*state }
    }

    /// The stored pointer, as the type the storage holds.
    fn stored(state: &State) -> *mut T::Base {
        state.ptr.get() as *mut T::Base
    }

    /// Extracts the value inside.
    ///
    /// Not a scheduling point: the storage is consumed, so no other task can reach it.
    pub fn into_inner(self) -> T {
        let me = std::mem::ManuallyDrop::new(self);
        // The initial value's count, which the storage owns.
        let initial = me.initial.swap(ptr::null_mut(), StdOrdering::AcqRel);
        // What this execution has stored, if it has stored anything.
        let stored = ExecutionState::try_with(|execution| {
            execution
                .get_storage::<_, State>(StorageKey(*me.id, STORAGE_TAG))
                .map(|state| (state.ptr.replace(ptr::null_mut()), state.owns.replace(false)))
        })
        .ok()
        .flatten();
        let ptr = match stored {
            // A value this execution stored: its count is ours, and the initial value's is given
            // up here, since nothing holds the storage any more.
            Some((ptr, true)) => {
                if !initial.is_null() {
                    // Safety: the storage owned this count.
                    unsafe { T::dec(initial) };
                }
                ptr as *mut T::Base
            }
            // The state still holds the initial value, whose count the storage owns.
            Some((_, false)) | None => initial,
        };
        // Safety: the count is ours now, and `me` is not dropped, so it does not take it too.
        unsafe { T::from_ptr(ptr) }
    }

    /// Replaces the value inside this instance.
    ///
    /// Further loads will yield the new value. Uses [`swap`](ArcSwapAny::swap) internally.
    ///
    /// Takes one scheduling point, and is one step (see the crate documentation).
    pub fn store(&self, val: T) {
        drop(self.swap(val));
    }

    /// Exchanges the value inside this instance.
    ///
    /// Takes one scheduling point, and is one step. Unlike arc-swap's, it never waits for the
    /// readers of the old value: a reader holds a clone of it, so it is freed when the last reader
    /// is done with it.
    pub fn swap(&self, new: T) -> T {
        thread::switch();
        let state = self.state();
        state.publish();
        let old = state.ptr.replace(T::into_ptr(new) as *mut ()) as *mut T::Base;
        if state.owns.replace(true) {
            // Safety: the reference count the state held for the old value is ours now.
            unsafe { T::from_ptr(old) }
        } else {
            // The old value is the storage's initial one, whose count the storage keeps, so the
            // caller gets a clone of it.
            // Safety: as in `load`.
            unsafe { clone_from_ptr::<T>(old) }
        }
    }

    /// Provides a temporary borrow of the object inside.
    ///
    /// This returns a proxy object allowing access to the thing held inside. However, there's
    /// only limited amount of possible cheap proxies in existence for each thread ‒ if more are
    /// created, it falls back to equivalent of [`load_full`](ArcSwapAny::load_full) internally.
    ///
    /// Under Shuttle there is no such limit: the guard holds a clone of the smart pointer, which
    /// keeps the value alive (see the crate documentation).
    ///
    /// Takes one scheduling point, and is one step.
    #[inline]
    pub fn load(&self) -> Guard<T, S> {
        thread::switch();
        let state = self.state();
        state.observe();
        // Safety: the state owns a reference count for its pointer, which stays valid until this
        // task runs again; `clone_from_ptr` leaves that count where it is.
        let value = unsafe { clone_from_ptr::<T>(Self::stored(state)) };
        Guard {
            inner: S::Protected::from_inner(value),
        }
    }

    /// Loads the value.
    ///
    /// This makes another copy of the held pointer and returns it, up-counting the reference count
    /// before.
    ///
    /// Takes one scheduling point, and is one step.
    pub fn load_full(&self) -> T {
        Guard::into_inner(self.load())
    }

    /// Replaces the content with a new value, if the old one is the expected one.
    ///
    /// Takes one scheduling point, and is one step, where arc-swap's loops until its compare and
    /// exchange succeeds or the values differ. The returned guard holds the value that was stored
    /// when the comparison was made: the old value if the exchange happened, and the value that is
    /// there if it did not. In the second case `new` is dropped, as in arc-swap.
    pub fn compare_and_swap<C>(&self, current: C, new: T) -> Guard<T, S>
    where
        C: AsRaw<T::Base>,
        S: CaS<T>,
    {
        thread::switch();
        let state = self.state();
        let stored = Self::stored(state);
        let value = if ptr::eq(stored, current.as_raw()) {
            state.publish();
            state.ptr.set(T::into_ptr(new) as *mut ());
            if state.owns.replace(true) {
                // Safety: the reference count the state held for the old value is ours now.
                unsafe { T::from_ptr(stored) }
            } else {
                // As in `swap`: the storage keeps the initial value's count.
                // Safety: as in `load`.
                unsafe { clone_from_ptr::<T>(stored) }
            }
        } else {
            state.observe();
            drop(new);
            // Safety: as in `load`.
            unsafe { clone_from_ptr::<T>(stored) }
        };
        drop(current);
        Guard {
            inner: S::Protected::from_inner(value),
        }
    }

    /// Read-Copy-Update of the pointer inside.
    ///
    /// This is useful in read-heavy situations with infrequent updates, but when the updates are
    /// small changes of the old value, not completely new values.
    ///
    /// The `f` is called with the old value and produces the new one. The exchange is attempted
    /// with [`compare_and_swap`](ArcSwapAny::compare_and_swap), and `f` runs again if another task
    /// changed the value in the meantime, so `f` may run more than once. It must not block or take
    /// a lock the writers of this storage can hold.
    ///
    /// Each attempt takes the scheduling points of a `load` and a `compare_and_swap`.
    pub fn rcu<R, F>(&self, mut f: F) -> T
    where
        F: FnMut(&T) -> R,
        R: Into<T>,
        S: CaS<T>,
    {
        let mut cur = self.load();
        loop {
            let new = f(&cur).into();
            let prev = self.compare_and_swap(&*cur, new);
            let swapped = ptr_eq(&*cur, &*prev);
            if swapped {
                return Guard::into_inner(prev);
            } else {
                cur = prev;
            }
        }
    }

    /// Provides an access to an up to date projection of the carried data.
    ///
    /// See [`access`] for details.
    pub fn map<I, R, F>(&self, f: F) -> Map<&Self, I, F>
    where
        F: Fn(&I) -> &R + Clone,
        Self: Access<I>,
    {
        Map::new(self, f)
    }
}

/// An atomic storage for `Arc`.
///
/// This is a type alias only. Most of its methods are described on [`ArcSwapAny`].
pub type ArcSwap<T> = ArcSwapAny<Arc<T>>;

impl<T, S: Strategy<Arc<T>>> ArcSwapAny<Arc<T>, S> {
    /// A convenience constructor directly from the pointed-to value.
    ///
    /// Direct equivalent for `ArcSwap::new(Arc::new(val))`.
    pub fn from_pointee(val: T) -> Self
    where
        S: Default,
    {
        Self::from(Arc::new(val))
    }
}

/// An atomic storage for `Option<Arc>`.
///
/// This is very similar to [`ArcSwap`], but allows storing NULL values, which is useful in some
/// situations.
///
/// This is a type alias only. Most of the methods are described on [`ArcSwapAny`]. Even though the
/// examples there often use [`ArcSwap`], they are applicable to `ArcSwapOption` with appropriate
/// changes.
pub type ArcSwapOption<T> = ArcSwapAny<Option<Arc<T>>>;

impl<T, S: Strategy<Option<Arc<T>>>> ArcSwapAny<Option<Arc<T>>, S> {
    /// A convenience constructor directly from a pointed-to value.
    ///
    /// This just allocates the `Arc` under the hood.
    pub fn from_pointee<V: Into<Option<T>>>(val: V) -> Self
    where
        S: Default,
    {
        Self::new(val.into().map(Arc::new))
    }

    /// A convenience constructor for an empty value.
    ///
    /// This is equivalent to `ArcSwapOption::new(None)`.
    pub fn empty() -> Self
    where
        S: Default,
    {
        Self::new(None)
    }
}

impl<T> ArcSwapOption<T> {
    /// A const-constructor for an empty value.
    ///
    /// Under Shuttle, a storage built this way is empty again at the start of every execution, so a
    /// `static` one does not carry a value from one execution into the next, which would make a
    /// failing schedule impossible to replay. The value the previous execution left is dropped at
    /// the first use in the next one.
    pub const fn const_empty() -> Self {
        Self {
            initial: StdAtomicPtr::new(ptr::null_mut()),
            id: StdLazyLock::new(next_id),
            _phantom_arc: PhantomData,
            strategy: ShuttleStrategyConst::VALUE,
        }
    }
}

/// `ShuttleStrategy`'s value, as a const, so that [`ArcSwapOption::const_empty`] can name it
/// without a const `Default`.
struct ShuttleStrategyConst;
impl ShuttleStrategyConst {
    const VALUE: DefaultStrategy = crate::strategy::ShuttleStrategy;
}

/// An atomic storage that doesn't share the internal generation locks with others.
///
/// This makes it slower, but means a slow reader won't block a writer of another storage. Under
/// Shuttle there are no such locks, so this is [`ArcSwap`].
#[doc(hidden)]
pub type IndependentArcSwap<T> = ArcSwapAny<Arc<T>, IndependentStrategy>;

/// Arc swap for the [`Weak`] pointer.
#[cfg(feature = "weak")]
#[cfg_attr(docsrs, doc(cfg(feature = "weak")))]
pub type ArcSwapWeak<T> = ArcSwapAny<std::sync::Weak<T>>;
