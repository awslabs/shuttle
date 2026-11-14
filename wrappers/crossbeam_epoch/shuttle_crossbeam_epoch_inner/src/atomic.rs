//! The pointer types: [`Atomic`], [`Owned`] and [`Shared`].
//!
//! The pointers are crossbeam's: a word holding an address from [`Pointable::init`] and a tag in the
//! bits the alignment leaves unused. What the model changes is where an [`Atomic`]'s word lives, and
//! that every operation on it is a scheduling point.

use crate::guard::Guard;
use crate::per_execution::PerExecution;
use shuttle::sync::atomic::{AtomicUsize, Ordering};
use std::borrow::{Borrow, BorrowMut};
use std::fmt;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};

#[allow(deprecated)]
pub use crossbeam_epoch_orig::CompareAndSetOrdering;
pub use crossbeam_epoch_orig::{Pointable, Pointer};

/// The error returned on failure of [`Atomic::compare_exchange`] and
/// [`Atomic::compare_exchange_weak`].
pub struct CompareExchangeError<'g, T: ?Sized + Pointable, P: Pointer<T>> {
    /// The value in the atomic pointer at the time of the failed operation.
    pub current: Shared<'g, T>,

    /// The new value, which the operation failed to store.
    pub new: P,
}

/// The error returned on failure of [`Atomic::compare_and_set`] and
/// [`Atomic::compare_and_set_weak`].
#[deprecated(note = "Use `CompareExchangeError` instead")]
pub type CompareAndSetError<'g, T, P> = CompareExchangeError<'g, T, P>;

impl<T: ?Sized + Pointable, P: Pointer<T> + fmt::Debug> fmt::Debug for CompareExchangeError<'_, T, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompareExchangeError")
            .field("current", &self.current)
            .field("new", &self.new)
            .finish()
    }
}

/// Returns a bitmask containing the unused least significant bits of an aligned pointer to `T`.
#[inline]
fn low_bits<T: ?Sized + Pointable>() -> usize {
    (1 << T::ALIGN.trailing_zeros()) - 1
}

/// Panics if the pointer is not properly unaligned.
#[inline]
fn ensure_aligned<T: ?Sized + Pointable>(raw: usize) {
    assert_eq!(raw & low_bits::<T>(), 0, "unaligned pointer");
}

/// Given a tagged pointer `data`, returns the same pointer, but tagged with `tag`.
///
/// `tag` is truncated to fit into the unused bits of the pointer to `T`.
#[inline]
fn compose_tag<T: ?Sized + Pointable>(data: usize, tag: usize) -> usize {
    (data & !low_bits::<T>()) | (tag & low_bits::<T>())
}

/// Decomposes a tagged pointer `data` into the pointer and the tag.
#[inline]
fn decompose_tag<T: ?Sized + Pointable>(data: usize) -> (usize, usize) {
    (data & !low_bits::<T>(), data & low_bits::<T>())
}

/// An atomic pointer that can be safely shared between threads, with the API of
/// [`crossbeam_epoch::Atomic`].
///
/// The word lives in the storage of the running execution, and starts every execution at the value
/// the pointer was constructed with, so a `static` `Atomic` is as constructed at the start of every
/// execution. Every operation on it is one scheduling point, and makes the calling task and the
/// pointer exchange vector clocks, as an operation on one of Shuttle's atomics does; `null`,
/// `tag`-only reads of a `Shared`, `Debug`, `Pointer` and dropping are not scheduling points.
///
/// Dropping an `Atomic` does not free what it points to, as in crossbeam.
///
/// [`crossbeam_epoch::Atomic`]: https://docs.rs/crossbeam-epoch/0.9/crossbeam_epoch/struct.Atomic.html
pub struct Atomic<T: ?Sized + Pointable> {
    /// The word every execution starts from.
    initial: usize,
    /// The word in the running execution.
    data: PerExecution<AtomicUsize>,
    _marker: PhantomData<*mut T>,
}

// Safety: the same bounds crossbeam's `Atomic` carries.
unsafe impl<T: ?Sized + Pointable + Send + Sync> Send for Atomic<T> {}
unsafe impl<T: ?Sized + Pointable + Send + Sync> Sync for Atomic<T> {}

impl<T> Atomic<T> {
    /// Allocates `value` on the heap and returns a new atomic pointer pointing to it.
    pub fn new(init: T) -> Atomic<T> {
        Self::init(init)
    }
}

impl<T: ?Sized + Pointable> Atomic<T> {
    /// Allocates `value` on the heap and returns a new atomic pointer pointing to it.
    pub fn init(init: T::Init) -> Atomic<T> {
        // Safety: `init` is `Pointable`'s own initializer, as crossbeam's `Atomic::init` passes it.
        Self::from_usize(unsafe { T::init(init) })
    }

    /// Returns a new atomic pointer pointing to the tagged pointer `data`.
    fn from_usize(data: usize) -> Self {
        Self {
            initial: data,
            data: PerExecution::new(),
            _marker: PhantomData,
        }
    }

    /// Returns a new null atomic pointer.
    pub const fn null() -> Atomic<T> {
        Self {
            initial: 0,
            data: PerExecution::new(),
            _marker: PhantomData,
        }
    }

    /// This execution's word, at its construction value if this execution has not touched it yet.
    ///
    /// The reference must not be kept across a scheduling point.
    fn data(&self) -> &AtomicUsize {
        let initial = self.initial;
        // The `RefCell` is borrowed only here, and the atomic's own bookkeeping takes the
        // execution's state, so the borrow must end before the operation runs. The atomic stays at
        // the slot's address for as long as the caller's borrow of `self` can last (see
        // `PerExecution::get`).
        let data: *const AtomicUsize = &*self.data.get(|| AtomicUsize::new(initial)).borrow();
        // Safety: the slot keeps the atomic at this address until the pointer is dropped or
        // consumed, which needs `&mut self` or `self`.
        unsafe { &*data }
    }

    /// Loads a `Shared` from the atomic pointer.
    ///
    /// This method takes an [`Ordering`] argument which describes the memory ordering of this
    /// operation. Shuttle models every ordering as `SeqCst`.
    pub fn load<'g>(&self, ord: Ordering, _: &'g Guard) -> Shared<'g, T> {
        // Safety: a word in an `Atomic<T>` came from `Pointable::init` or is null.
        unsafe { Shared::from_usize(self.data().load(ord)) }
    }

    /// Loads a `Shared` from the atomic pointer using a "consume" memory ordering.
    ///
    /// Shuttle models every ordering as `SeqCst`, so this is [`load`](Self::load) with `Acquire`.
    pub fn load_consume<'g>(&self, _: &'g Guard) -> Shared<'g, T> {
        // Safety: as in `load`.
        unsafe { Shared::from_usize(self.data().load(Ordering::Acquire)) }
    }

    /// Stores a `Shared` or `Owned` pointer into the atomic pointer.
    pub fn store<P: Pointer<T>>(&self, new: P, ord: Ordering) {
        self.data().store(new.into_usize(), ord);
    }

    /// Stores a `Shared` or `Owned` pointer into the atomic pointer, returning the previous
    /// `Shared`.
    pub fn swap<'g, P: Pointer<T>>(&self, new: P, ord: Ordering, _: &'g Guard) -> Shared<'g, T> {
        // Safety: as in `load`.
        unsafe { Shared::from_usize(self.data().swap(new.into_usize(), ord)) }
    }

    /// Stores the pointer `new` (either `Shared` or `Owned`) into the atomic pointer if the current
    /// value is the same as `current`. The tag is also taken into account, so two pointers to the
    /// same object, but with different tags, will not be considered equal.
    ///
    /// The return value is a result indicating whether the new pointer was written. On success the
    /// pointer that was written is returned. On failure the actual current value and `new` are
    /// returned.
    pub fn compare_exchange<'g, P>(
        &self,
        current: Shared<'_, T>,
        new: P,
        success: Ordering,
        failure: Ordering,
        _: &'g Guard,
    ) -> Result<Shared<'g, T>, CompareExchangeError<'g, T, P>>
    where
        P: Pointer<T>,
    {
        let new = new.into_usize();
        self.data()
            .compare_exchange(current.into_usize(), new, success, failure)
            // Safety: as in `load`.
            .map(|_| unsafe { Shared::from_usize(new) })
            .map_err(|current| CompareExchangeError {
                // Safety: as in `load`.
                current: unsafe { Shared::from_usize(current) },
                // Safety: `new` came from `P::into_usize` just above, and was not written.
                new: unsafe { P::from_usize(new) },
            })
    }

    /// Stores the pointer `new` (either `Shared` or `Owned`) into the atomic pointer if the current
    /// value is the same as `current`. The tag is also taken into account, so two pointers to the
    /// same object, but with different tags, will not be considered equal.
    ///
    /// Unlike [`compare_exchange`], this method is allowed to spuriously fail even when comparison
    /// succeeds, which can result in more efficient code on some platforms. The return value is a
    /// result indicating whether the new pointer was written. On success the pointer that was
    /// written is returned. On failure the actual current value and `new` are returned.
    ///
    /// Shuttle's atomics do not fail spuriously, so under Shuttle this is `compare_exchange`.
    ///
    /// [`compare_exchange`]: Atomic::compare_exchange
    pub fn compare_exchange_weak<'g, P>(
        &self,
        current: Shared<'_, T>,
        new: P,
        success: Ordering,
        failure: Ordering,
        guard: &'g Guard,
    ) -> Result<Shared<'g, T>, CompareExchangeError<'g, T, P>>
    where
        P: Pointer<T>,
    {
        self.compare_exchange(current, new, success, failure, guard)
    }

    /// Fetches the pointer, and then applies a function to it that returns a new value. Returns a
    /// `Result` of `Ok(previous_value)` if the function returned `Some`, else `Err(_)`.
    ///
    /// Note: This may call the function multiple times if the value has been changed from other
    /// threads in the meantime, as long as the function returns `Some(_)`, but the function will
    /// have been applied only once to the stored value.
    pub fn fetch_update<'g, F>(
        &self,
        set_order: Ordering,
        fail_order: Ordering,
        guard: &'g Guard,
        mut func: F,
    ) -> Result<Shared<'g, T>, Shared<'g, T>>
    where
        F: FnMut(Shared<'g, T>) -> Option<Shared<'g, T>>,
    {
        let mut prev = self.load(fail_order, guard);
        while let Some(next) = func(prev) {
            match self.compare_exchange_weak(prev, next, set_order, fail_order, guard) {
                // crossbeam returns the previous pointer here, not the one it wrote.
                Ok(_written) => return Ok(prev),
                Err(next_prev) => prev = next_prev.current,
            }
        }
        Err(prev)
    }

    /// Stores the pointer `new` (either `Shared` or `Owned`) into the atomic pointer if the current
    /// value is the same as `current`. The tag is also taken into account, so two pointers to the
    /// same object, but with different tags, will not be considered equal.
    #[allow(deprecated)]
    #[deprecated(note = "Use `compare_exchange` instead")]
    pub fn compare_and_set<'g, O, P>(
        &self,
        current: Shared<'_, T>,
        new: P,
        ord: O,
        guard: &'g Guard,
    ) -> Result<Shared<'g, T>, CompareAndSetError<'g, T, P>>
    where
        O: CompareAndSetOrdering,
        P: Pointer<T>,
    {
        self.compare_exchange(current, new, ord.success(), ord.failure(), guard)
    }

    /// Stores the pointer `new` (either `Shared` or `Owned`) into the atomic pointer if the current
    /// value is the same as `current`. The tag is also taken into account, so two pointers to the
    /// same object, but with different tags, will not be considered equal.
    ///
    /// Unlike [`compare_and_set`], this method is allowed to spuriously fail even when comparison
    /// succeeds, which can result in more efficient code on some platforms. Shuttle's atomics do
    /// not fail spuriously.
    ///
    /// [`compare_and_set`]: Atomic::compare_and_set
    #[allow(deprecated)]
    #[deprecated(note = "Use `compare_exchange_weak` instead")]
    pub fn compare_and_set_weak<'g, O, P>(
        &self,
        current: Shared<'_, T>,
        new: P,
        ord: O,
        guard: &'g Guard,
    ) -> Result<Shared<'g, T>, CompareAndSetError<'g, T, P>>
    where
        O: CompareAndSetOrdering,
        P: Pointer<T>,
    {
        self.compare_exchange_weak(current, new, ord.success(), ord.failure(), guard)
    }

    /// Bitwise "and" with the current tag.
    ///
    /// Performs a bitwise "and" operation on the current tag and the argument `val`, and sets the
    /// new tag to the result. Returns the previous pointer.
    pub fn fetch_and<'g>(&self, val: usize, ord: Ordering, _: &'g Guard) -> Shared<'g, T> {
        // Safety: as in `load`.
        unsafe { Shared::from_usize(self.data().fetch_and(val | !low_bits::<T>(), ord)) }
    }

    /// Bitwise "or" with the current tag.
    ///
    /// Performs a bitwise "or" operation on the current tag and the argument `val`, and sets the new
    /// tag to the result. Returns the previous pointer.
    pub fn fetch_or<'g>(&self, val: usize, ord: Ordering, _: &'g Guard) -> Shared<'g, T> {
        // Safety: as in `load`.
        unsafe { Shared::from_usize(self.data().fetch_or(val & low_bits::<T>(), ord)) }
    }

    /// Bitwise "xor" with the current tag.
    ///
    /// Performs a bitwise "xor" operation on the current tag and the argument `val`, and sets the
    /// new tag to the result. Returns the previous pointer.
    pub fn fetch_xor<'g>(&self, val: usize, ord: Ordering, _: &'g Guard) -> Shared<'g, T> {
        // Safety: as in `load`.
        unsafe { Shared::from_usize(self.data().fetch_xor(val & low_bits::<T>(), ord)) }
    }

    /// Takes ownership of the pointee.
    ///
    /// # Safety
    ///
    /// This method may be called only if the pointer is valid and nobody else is holding a reference
    /// to the same object.
    pub unsafe fn into_owned(self) -> Owned<T> {
        // Safety: the caller promises that the pointer is valid and unshared.
        unsafe { Owned::from_usize(self.take()) }
    }

    /// Takes ownership of the pointee if it is non-null.
    ///
    /// # Safety
    ///
    /// This method may be called only if the pointer is valid and nobody else is holding a reference
    /// to the same object, or the pointer is null.
    pub unsafe fn try_into_owned(self) -> Option<Owned<T>> {
        let data = self.take();
        if decompose_tag::<T>(data).0 == 0 {
            None
        } else {
            // Safety: the caller promises that the pointer is valid and unshared.
            Some(unsafe { Owned::from_usize(data) })
        }
    }

    /// This execution's word, consuming the pointer. Outside of a Shuttle execution, and while one
    /// is being torn down, the construction value.
    fn take(mut self) -> usize {
        match self.data.take() {
            Some(data) => data.into_inner(),
            None => self.initial,
        }
    }
}

impl<T: ?Sized + Pointable> Atomic<T> {
    /// A snapshot of the word, which `Debug`, `Pointer` and `Clone` use. Reads the execution's word
    /// inside a Shuttle execution, and the pointer's construction value outside of one, so these
    /// three impls work anywhere.
    fn snapshot(&self) -> usize {
        shuttle_engine::runtime::execution::ExecutionState::try_with(|_| ())
            .map(|_| self.data().load(Ordering::SeqCst))
            .unwrap_or(self.initial)
    }
}

impl<T: ?Sized + Pointable> fmt::Debug for Atomic<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (raw, tag) = decompose_tag::<T>(self.snapshot());

        f.debug_struct("Atomic").field("raw", &raw).field("tag", &tag).finish()
    }
}

impl<T: ?Sized + Pointable> fmt::Pointer for Atomic<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (raw, _) = decompose_tag::<T>(self.snapshot());
        fmt::Pointer::fmt(&(unsafe { T::deref(raw) as *const _ }), f)
    }
}

impl<T: ?Sized + Pointable> Clone for Atomic<T> {
    /// Returns a copy of the atomic value.
    ///
    /// Note that a `Relaxed` load is used here. If you need synchronization, use it with other
    /// atomics or fences.
    ///
    /// Inside an execution, the copy starts every later execution at the word this one reads here;
    /// outside of one, both start at the pointer's construction value.
    fn clone(&self) -> Self {
        Atomic::from_usize(self.snapshot())
    }
}

impl<T: ?Sized + Pointable> Default for Atomic<T> {
    fn default() -> Self {
        Atomic::null()
    }
}

impl<T: ?Sized + Pointable> From<Owned<T>> for Atomic<T> {
    /// Returns a new atomic pointer pointing to `owned`.
    fn from(owned: Owned<T>) -> Self {
        let data = owned.data;
        std::mem::forget(owned);
        Self::from_usize(data)
    }
}

impl<T> From<Box<T>> for Atomic<T> {
    fn from(b: Box<T>) -> Self {
        Self::from(Owned::from(b))
    }
}

impl<T> From<T> for Atomic<T> {
    fn from(t: T) -> Self {
        Self::new(t)
    }
}

impl<'g, T: ?Sized + Pointable> From<Shared<'g, T>> for Atomic<T> {
    /// Returns a new atomic pointer pointing to `ptr`.
    fn from(ptr: Shared<'g, T>) -> Self {
        Self::from_usize(ptr.data)
    }
}

impl<T> From<*const T> for Atomic<T> {
    /// Returns a new atomic pointer pointing to `raw`.
    fn from(raw: *const T) -> Self {
        Self::from_usize(raw as usize)
    }
}

/// An owned heap-allocated object, with the API of [`crossbeam_epoch::Owned`].
///
/// This type is very similar to `Box<T>`, and the model is crossbeam's: the pointer is an address
/// from [`Pointable::init`] and a tag. No operation on it is a scheduling point, and it can be used
/// outside of a Shuttle execution.
///
/// [`crossbeam_epoch::Owned`]: https://docs.rs/crossbeam-epoch/0.9/crossbeam_epoch/struct.Owned.html
pub struct Owned<T: ?Sized + Pointable> {
    data: usize,
    _marker: PhantomData<Box<T>>,
}

impl<T: ?Sized + Pointable> Pointer<T> for Owned<T> {
    #[inline]
    fn into_usize(self) -> usize {
        let data = self.data;
        std::mem::forget(self);
        data
    }

    /// Returns a new pointer pointing to the tagged pointer `data`.
    ///
    /// # Panics
    ///
    /// Panics if the data is zero in debug mode.
    #[inline]
    unsafe fn from_usize(data: usize) -> Self {
        debug_assert!(data != 0, "converting zero into `Owned`");
        Owned {
            data,
            _marker: PhantomData,
        }
    }
}

impl<T> Owned<T> {
    /// Returns a new owned pointer pointing to `raw`.
    ///
    /// This function is unsafe because improper use may lead to memory problems. Argument `raw` must
    /// be a valid pointer. Also, a double-free may occur if the function is called twice on the same
    /// raw pointer.
    ///
    /// # Panics
    ///
    /// Panics if `raw` is not properly aligned.
    ///
    /// # Safety
    ///
    /// The given `raw` should have been derived from `Owned`, and one `raw` should not be converted
    /// back by `Owned::from_raw()` multiple times.
    pub unsafe fn from_raw(raw: *mut T) -> Owned<T> {
        let raw = raw as usize;
        ensure_aligned::<T>(raw);
        // Safety: the caller promises a pointer from an `Owned`, which is not null.
        unsafe { Self::from_usize(raw) }
    }

    /// Converts the owned pointer into a `Box`.
    pub fn into_box(self) -> Box<T> {
        let (raw, _) = decompose_tag::<T>(self.data);
        std::mem::forget(self);
        // Safety: the address came from `Pointable::init` for a sized `T`, which boxes it.
        unsafe { Box::from_raw(raw as *mut _) }
    }

    /// Allocates `value` on the heap and returns a new owned pointer pointing to it.
    pub fn new(init: T) -> Owned<T> {
        Self::init(init)
    }
}

impl<T: ?Sized + Pointable> Owned<T> {
    /// Allocates `value` on the heap and returns a new owned pointer pointing to it.
    pub fn init(init: T::Init) -> Owned<T> {
        // Safety: `init` is `Pointable`'s own initializer, as crossbeam's `Owned::init` passes it.
        unsafe { Self::from_usize(T::init(init)) }
    }

    /// Converts the owned pointer into a [`Shared`].
    pub fn into_shared<'g>(self, _: &'g Guard) -> Shared<'g, T> {
        // Safety: the word came from this `Owned`, which gives up its ownership.
        unsafe { Shared::from_usize(self.into_usize()) }
    }

    /// Returns the tag stored within the pointer.
    pub fn tag(&self) -> usize {
        let (_, tag) = decompose_tag::<T>(self.data);
        tag
    }

    /// Returns the same pointer, but tagged with `tag`. `tag` is truncated to be fit into the
    /// unused bits of the pointer to `T`.
    pub fn with_tag(self, tag: usize) -> Owned<T> {
        let data = self.into_usize();
        // Safety: the word came from an `Owned`, and tagging it leaves the address alone.
        unsafe { Self::from_usize(compose_tag::<T>(data, tag)) }
    }
}

impl<T: ?Sized + Pointable> Drop for Owned<T> {
    fn drop(&mut self) {
        let (raw, _) = decompose_tag::<T>(self.data);
        // Safety: this `Owned` owns the object, and this runs once.
        unsafe {
            T::drop(raw);
        }
    }
}

impl<T: ?Sized + Pointable> fmt::Debug for Owned<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (raw, tag) = decompose_tag::<T>(self.data);

        f.debug_struct("Owned").field("raw", &raw).field("tag", &tag).finish()
    }
}

impl<T: Clone> Clone for Owned<T> {
    fn clone(&self) -> Self {
        Owned::new((**self).clone()).with_tag(self.tag())
    }
}

impl<T: ?Sized + Pointable> Deref for Owned<T> {
    type Target = T;

    fn deref(&self) -> &T {
        let (raw, _) = decompose_tag::<T>(self.data);
        // Safety: this `Owned` owns a live object, and holds a shared borrow of it.
        unsafe { T::deref(raw) }
    }
}

impl<T: ?Sized + Pointable> DerefMut for Owned<T> {
    fn deref_mut(&mut self) -> &mut T {
        let (raw, _) = decompose_tag::<T>(self.data);
        // Safety: this `Owned` owns a live object, and holds a unique borrow of it.
        unsafe { T::deref_mut(raw) }
    }
}

impl<T> From<T> for Owned<T> {
    fn from(t: T) -> Self {
        Owned::new(t)
    }
}

impl<T> From<Box<T>> for Owned<T> {
    /// Returns a new owned pointer pointing to `b`.
    ///
    /// # Panics
    ///
    /// Panics if the pointer (the `Box`) is not properly aligned.
    fn from(b: Box<T>) -> Self {
        // Safety: a `Box` holds a live object, and gives up its ownership.
        unsafe { Self::from_raw(Box::into_raw(b)) }
    }
}

impl<T: ?Sized + Pointable> Borrow<T> for Owned<T> {
    fn borrow(&self) -> &T {
        self
    }
}

impl<T: ?Sized + Pointable> BorrowMut<T> for Owned<T> {
    fn borrow_mut(&mut self) -> &mut T {
        self
    }
}

impl<T: ?Sized + Pointable> AsRef<T> for Owned<T> {
    fn as_ref(&self) -> &T {
        self
    }
}

impl<T: ?Sized + Pointable> AsMut<T> for Owned<T> {
    fn as_mut(&mut self) -> &mut T {
        self
    }
}

/// A pointer to an object protected by the epoch GC, with the API of [`crossbeam_epoch::Shared`].
///
/// The pointer is valid for use only during the lifetime `'g`. The model is crossbeam's, with one
/// difference that matters: deferred destruction never runs, so the object a `Shared` points to is
/// still allocated for the rest of the process (see the [crate documentation](crate)).
///
/// [`crossbeam_epoch::Shared`]: https://docs.rs/crossbeam-epoch/0.9/crossbeam_epoch/struct.Shared.html
pub struct Shared<'g, T: 'g + ?Sized + Pointable> {
    data: usize,
    _marker: PhantomData<(&'g (), *const T)>,
}

impl<T: ?Sized + Pointable> Clone for Shared<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized + Pointable> Copy for Shared<'_, T> {}

impl<T: ?Sized + Pointable> Pointer<T> for Shared<'_, T> {
    #[inline]
    fn into_usize(self) -> usize {
        self.data
    }

    #[inline]
    unsafe fn from_usize(data: usize) -> Self {
        Shared {
            data,
            _marker: PhantomData,
        }
    }
}

impl<T> Shared<'_, T> {
    /// Converts the pointer to a raw pointer (without the tag).
    pub fn as_raw(&self) -> *const T {
        let (raw, _) = decompose_tag::<T>(self.data);
        raw as *const _
    }
}

impl<'g, T: ?Sized + Pointable> Shared<'g, T> {
    /// Returns a new null pointer.
    pub const fn null() -> Shared<'g, T> {
        Shared {
            data: 0,
            _marker: PhantomData,
        }
    }

    /// Returns `true` if the pointer is null.
    pub fn is_null(&self) -> bool {
        let (raw, _) = decompose_tag::<T>(self.data);
        raw == 0
    }

    /// Dereferences the pointer.
    ///
    /// Returns a reference to the pointee that is valid during the lifetime `'g`.
    ///
    /// # Safety
    ///
    /// Dereferencing a pointer is unsafe because it could be pointing to invalid memory.
    ///
    /// Another concern is the possibility of data races due to lack of proper synchronization. For
    /// example, consider the following scenario:
    ///
    /// 1. A thread creates a new object: `a.store(Owned::new(10), Relaxed)`
    /// 2. Another thread reads it: `*a.load(Relaxed, guard).as_ref().unwrap()`
    ///
    /// The problem is that relaxed orderings don't synchronize initialization of the object with
    /// the read from the second thread. This is a data race. A possible solution would be to use
    /// `Release` and `Acquire` orderings. Shuttle models every ordering as `SeqCst`, so it does not
    /// find bugs that need a relaxed ordering to happen.
    pub unsafe fn deref(&self) -> &'g T {
        let (raw, _) = decompose_tag::<T>(self.data);
        // Safety: the caller promises that the pointer is valid and unaliased.
        unsafe { T::deref(raw) }
    }

    /// Dereferences the pointer.
    ///
    /// Returns a mutable reference to the pointee that is valid during the lifetime `'g`.
    ///
    /// # Safety
    ///
    /// * There is no guarantee that there are no more threads attempting to read/write from/to the
    ///   actual object at the same time.
    ///
    ///   The user must know that there are no concurrent accesses towards the object itself.
    ///
    /// * Other than the above, all safety concerns of `deref()` applies here.
    #[allow(clippy::should_implement_trait)]
    pub unsafe fn deref_mut(&mut self) -> &'g mut T {
        let (raw, _) = decompose_tag::<T>(self.data);
        // Safety: the caller promises that the pointer is valid and that it alone reaches it.
        unsafe { T::deref_mut(raw) }
    }

    /// Converts the pointer to a reference.
    ///
    /// Returns `None` if the pointer is null, or else a reference to the object wrapped in `Some`.
    ///
    /// # Safety
    ///
    /// The same safety concerns as [`deref`](Self::deref) apply.
    pub unsafe fn as_ref(&self) -> Option<&'g T> {
        let (raw, _) = decompose_tag::<T>(self.data);
        if raw == 0 {
            None
        } else {
            // Safety: the caller promises that the pointer is valid and unaliased.
            Some(unsafe { T::deref(raw) })
        }
    }

    /// Takes ownership of the pointee.
    ///
    /// # Panics
    ///
    /// Panics if this pointer is null, but only in debug mode.
    ///
    /// # Safety
    ///
    /// This method may be called only if the pointer is valid and nobody else is holding a reference
    /// to the same object.
    pub unsafe fn into_owned(self) -> Owned<T> {
        debug_assert!(!self.is_null(), "converting a null `Shared` into `Owned`");
        // Safety: the caller promises that the pointer is valid and unshared.
        unsafe { Owned::from_usize(self.data) }
    }

    /// Takes ownership of the pointee if it is not null.
    ///
    /// # Safety
    ///
    /// This method may be called only if the pointer is valid and nobody else is holding a reference
    /// to the same object, or if the pointer is null.
    pub unsafe fn try_into_owned(self) -> Option<Owned<T>> {
        if self.is_null() {
            None
        } else {
            // Safety: the caller promises that the pointer is valid and unshared.
            Some(unsafe { Owned::from_usize(self.data) })
        }
    }

    /// Returns the tag stored within the pointer.
    pub fn tag(&self) -> usize {
        let (_, tag) = decompose_tag::<T>(self.data);
        tag
    }

    /// Returns the same pointer, but tagged with `tag`. `tag` is truncated to be fit into the
    /// unused bits of the pointer to `T`.
    pub fn with_tag(&self, tag: usize) -> Shared<'g, T> {
        // Safety: tagging a word leaves its address alone.
        unsafe { Self::from_usize(compose_tag::<T>(self.data, tag)) }
    }
}

impl<T> From<*const T> for Shared<'_, T> {
    /// Returns a new pointer pointing to `raw`.
    ///
    /// # Panics
    ///
    /// Panics if `raw` is not properly aligned.
    fn from(raw: *const T) -> Self {
        let raw = raw as usize;
        ensure_aligned::<T>(raw);
        // Safety: an aligned address of a `T`, as crossbeam's `From` takes.
        unsafe { Self::from_usize(raw) }
    }
}

impl<'g, T: ?Sized + Pointable> PartialEq<Shared<'g, T>> for Shared<'g, T> {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}

impl<T: ?Sized + Pointable> Eq for Shared<'_, T> {}

impl<'g, T: ?Sized + Pointable> PartialOrd<Shared<'g, T>> for Shared<'g, T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: ?Sized + Pointable> Ord for Shared<'_, T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.data.cmp(&other.data)
    }
}

impl<T: ?Sized + Pointable> fmt::Debug for Shared<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (raw, tag) = decompose_tag::<T>(self.data);

        f.debug_struct("Shared").field("raw", &raw).field("tag", &tag).finish()
    }
}

impl<T: ?Sized + Pointable> fmt::Pointer for Shared<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Pointer::fmt(&(unsafe { self.deref() as *const _ }), f)
    }
}

impl<T: ?Sized + Pointable> Default for Shared<'_, T> {
    fn default() -> Self {
        Shared::null()
    }
}
