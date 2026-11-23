use super::checks::{self, Access, Alloc, Reference, Track};
use std::fmt;
use std::marker::PhantomData;
use std::mem::{size_of, ManuallyDrop};
use std::ops::{Deref, DerefMut};
use std::panic::Location;

/// A version of [`std::cell::UnsafeCell`] whose value is reached through a [`Ptr`]. See the
/// [module documentation](super).
pub struct UnsafeCell<T> {
    alloc: Alloc,
    value: std::cell::UnsafeCell<T>,
}

impl<T> UnsafeCell<T> {
    /// Makes a new cell holding `value`. With the checks, this counts as a write of the value by the
    /// current task, as in Miri, so an access that doesn't happen after it is a data race.
    #[track_caller]
    pub fn new(value: T) -> Self {
        Self {
            alloc: Alloc::new::<T>(),
            value: std::cell::UnsafeCell::new(value),
        }
    }

    /// A pointer to the value, as [`std::cell::UnsafeCell::get`] returns.
    pub fn get(&self) -> Ptr<T> {
        Ptr {
            raw: self.value.get(),
            track: self.alloc.track::<T>(false),
        }
    }

    /// Calls `f` with a raw pointer to the value, after a check of a read through it. This is
    /// loom's `UnsafeCell::with`.
    #[track_caller]
    pub fn with<R>(&self, f: impl FnOnce(*const T) -> R) -> R {
        checks::access::<T>(self.alloc.track::<T>(false), Access::Read, Location::caller());
        f(self.value.get())
    }

    /// Calls `f` with a raw pointer to the value, after a check of a write through it. This is
    /// loom's `UnsafeCell::with_mut`.
    #[track_caller]
    pub fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        checks::access::<T>(self.alloc.track::<T>(false), Access::Write, Location::caller());
        f(self.value.get())
    }

    /// A mutable reference to the value, as [`std::cell::UnsafeCell::get_mut`] returns. This counts
    /// as a write through the cell itself, which disables every reference made from the cell
    /// before. It isn't checked for a data race: the caller has the cell to itself, which a hand-off
    /// that Shuttle doesn't see may have arranged, such as the last drop of a std `Arc`. The write
    /// is recorded, so a later access by another task must still happen after it.
    #[track_caller]
    pub fn get_mut(&mut self) -> &mut T {
        checks::access_unordered::<T>(self.alloc.track::<T>(true), Access::Write, Location::caller());
        self.value.get_mut()
    }

    /// Unwraps the value. This counts as a read through the cell itself. As with `get_mut`, it isn't
    /// checked for a data race, but it is recorded.
    #[track_caller]
    pub fn into_inner(self) -> T {
        checks::access_unordered::<T>(self.alloc.track::<T>(true), Access::Read, Location::caller());
        self.alloc.free();
        let this = ManuallyDrop::new(self);
        // SAFETY: `this` is never used or dropped again.
        unsafe { std::ptr::read(&this.value) }.into_inner()
    }
}

impl<T> Drop for UnsafeCell<T> {
    fn drop(&mut self) {
        self.alloc.free();
    }
}

impl<T: Default> Default for UnsafeCell<T> {
    #[track_caller]
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> fmt::Debug for UnsafeCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnsafeCell").finish_non_exhaustive()
    }
}

/// A raw pointer into an [`UnsafeCell`], which carries its tag. It is `Copy` and neither `Send`
/// nor `Sync`, as a raw pointer is.
pub struct Ptr<T> {
    raw: *mut T,
    track: Option<Track>,
}

impl<T> Clone for Ptr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Ptr<T> {}

impl<T> fmt::Debug for Ptr<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match checks::tag(self.track) {
            Some(tag) => write!(f, "Ptr({:?}, tag #{tag})", self.raw),
            None => write!(f, "Ptr({:?})", self.raw),
        }
    }
}

impl<T> Ptr<T> {
    /// The plain raw pointer. Shuttle doesn't see the accesses through it.
    pub fn as_raw(self) -> *mut T {
        self.raw
    }

    /// The same pointer, to a `U`. It keeps its tag.
    pub fn cast<U>(self) -> Ptr<U> {
        Ptr {
            raw: self.raw.cast(),
            track: self.track,
        }
    }

    /// A pointer to a part of the value, such as a field, with the same tag. `f` gets the raw
    /// pointer and returns one into the same value, as `&raw mut (*p).field` does. That makes no
    /// reference, so nothing is checked until an access through the new pointer.
    ///
    /// ```
    /// # use shuttle_std::cell::UnsafeCell;
    /// struct Pair {
    ///     a: u32,
    ///     b: u64,
    /// }
    ///
    /// shuttle_schedulers::check_dfs(
    ///     || {
    ///         let cell = UnsafeCell::new(Pair { a: 1, b: 2 });
    ///         let b = cell.get().project(|p| unsafe { &raw mut (*p).b });
    ///         unsafe { b.write(3) };
    ///         assert_eq!(unsafe { b.read() }, 3);
    ///     },
    ///     None,
    /// );
    /// ```
    pub fn project<U>(self, f: impl FnOnce(*mut T) -> *mut U) -> Ptr<U> {
        let raw = f(self.raw);
        let offset = raw.addr().wrapping_sub(self.raw.addr());
        Ptr {
            raw,
            track: checks::offset(self.track, offset),
        }
    }

    /// The pointer `count` values of `T` further on, as [`pointer::add`] gives.
    ///
    /// # Safety
    ///
    /// As for [`pointer::add`].
    ///
    /// [`pointer::add`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.add
    pub unsafe fn add(self, count: usize) -> Self {
        // SAFETY: the caller's.
        unsafe { self.byte_add(count * size_of::<T>()) }
    }

    /// The pointer `count` bytes further on, as [`pointer::byte_add`] gives.
    ///
    /// # Safety
    ///
    /// As for [`pointer::byte_add`].
    ///
    /// [`pointer::byte_add`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.byte_add
    pub unsafe fn byte_add(self, count: usize) -> Self {
        Ptr {
            // SAFETY: the caller's.
            raw: unsafe { self.raw.byte_add(count) },
            track: checks::offset(self.track, count),
        }
    }

    /// Reads the value, as [`pointer::read`] does.
    ///
    /// # Safety
    ///
    /// As for [`pointer::read`].
    ///
    /// [`pointer::read`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.read
    #[track_caller]
    pub unsafe fn read(self) -> T {
        checks::access::<T>(self.track, Access::Read, Location::caller());
        // SAFETY: the caller's.
        unsafe { self.raw.read() }
    }

    /// Writes the value, as [`pointer::write`] does.
    ///
    /// # Safety
    ///
    /// As for [`pointer::write`].
    ///
    /// [`pointer::write`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.write
    #[track_caller]
    pub unsafe fn write(self, value: T) {
        checks::access::<T>(self.track, Access::Write, Location::caller());
        // SAFETY: the caller's.
        unsafe { self.raw.write(value) }
    }

    /// Replaces the value, as [`pointer::replace`] does.
    ///
    /// # Safety
    ///
    /// As for [`pointer::replace`].
    ///
    /// [`pointer::replace`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.replace
    #[track_caller]
    pub unsafe fn replace(self, value: T) -> T {
        checks::access::<T>(self.track, Access::Write, Location::caller());
        // SAFETY: the caller's.
        unsafe { self.raw.replace(value) }
    }

    /// Makes a shared reference, as `&*ptr` does. With the checks, it gets a new tag, which starts
    /// `Frozen`.
    ///
    /// # Safety
    ///
    /// As for `&*ptr`, except that Shuttle reports a violation of Tree Borrows or a data race.
    #[track_caller]
    pub unsafe fn as_ref<'a>(self) -> PtrRef<'a, T> {
        PtrRef {
            raw: self.raw,
            track: checks::retag::<T>(self.track, Reference::Shared, Location::caller()),
            raw_made: std::cell::Cell::new(false),
            _marker: PhantomData,
        }
    }

    /// Makes a mutable reference, as `&mut *ptr` does. With the checks, it gets a new tag, which
    /// starts `Reserved`.
    ///
    /// # Safety
    ///
    /// As for `&mut *ptr`, except that Shuttle reports a violation of Tree Borrows or a data race.
    #[track_caller]
    pub unsafe fn as_mut<'a>(self) -> PtrRefMut<'a, T> {
        PtrRefMut {
            raw: self.raw,
            track: checks::retag::<T>(self.track, Reference::Mut, Location::caller()),
            raw_made: false,
            _marker: PhantomData,
        }
    }
}

/// A shared reference into an [`UnsafeCell`], which [`Ptr::as_ref`] makes. With the checks, each
/// dereference is checked as a read.
pub struct PtrRef<'a, T> {
    raw: *const T,
    track: Option<Track>,
    raw_made: std::cell::Cell<bool>,
    _marker: PhantomData<&'a T>,
}

impl<T> PtrRef<'_, T> {
    /// A raw pointer with this reference's tag, as `r as *const T` gives. Writing through it is UB,
    /// as it is for the raw pointer.
    pub fn as_ptr(this: &Self) -> Ptr<T> {
        this.raw_made.set(true);
        Ptr {
            raw: this.raw.cast_mut(),
            track: this.track,
        }
    }
}

impl<T> Deref for PtrRef<'_, T> {
    type Target = T;

    #[track_caller]
    fn deref(&self) -> &T {
        checks::access::<T>(self.track, Access::Read, Location::caller());
        // SAFETY: `Ptr::as_ref`'s caller promised that this is a valid reference.
        unsafe { &*self.raw }
    }
}

impl<T> Drop for PtrRef<'_, T> {
    fn drop(&mut self) {
        if !self.raw_made.get() {
            checks::release(self.track);
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for PtrRef<'_, T> {
    #[track_caller]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

/// A mutable reference into an [`UnsafeCell`], which [`Ptr::as_mut`] makes. With the checks, each
/// `Deref` is checked as a read, and each `DerefMut` as a write.
pub struct PtrRefMut<'a, T> {
    raw: *mut T,
    track: Option<Track>,
    raw_made: bool,
    _marker: PhantomData<&'a mut T>,
}

impl<T> PtrRefMut<'_, T> {
    /// A raw pointer with this reference's tag, as `r as *mut T` gives.
    pub fn as_ptr(this: &mut Self) -> Ptr<T> {
        this.raw_made = true;
        Ptr {
            raw: this.raw,
            track: this.track,
        }
    }
}

impl<T> Deref for PtrRefMut<'_, T> {
    type Target = T;

    #[track_caller]
    fn deref(&self) -> &T {
        checks::access::<T>(self.track, Access::Read, Location::caller());
        // SAFETY: `Ptr::as_mut`'s caller promised that this is a valid reference.
        unsafe { &*self.raw }
    }
}

impl<T> DerefMut for PtrRefMut<'_, T> {
    #[track_caller]
    fn deref_mut(&mut self) -> &mut T {
        checks::access::<T>(self.track, Access::Write, Location::caller());
        // SAFETY: `Ptr::as_mut`'s caller promised that this is a valid reference.
        unsafe { &mut *self.raw }
    }
}

impl<T> Drop for PtrRefMut<'_, T> {
    fn drop(&mut self) {
        if !self.raw_made {
            checks::release(self.track);
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for PtrRefMut<'_, T> {
    #[track_caller]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}
