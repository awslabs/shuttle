use std::fmt;
use std::marker::PhantomData;
use std::mem::size_of;
use std::ops::{Deref, DerefMut};

/// A version of [`std::cell::UnsafeCell`] whose value is reached through a [`Ptr`]. See the
/// [module documentation](super).
pub struct UnsafeCell<T> {
    value: std::cell::UnsafeCell<T>,
}

impl<T> UnsafeCell<T> {
    /// Makes a new cell holding `value`.
    pub fn new(value: T) -> Self {
        Self {
            value: std::cell::UnsafeCell::new(value),
        }
    }

    /// A pointer to the value, as [`std::cell::UnsafeCell::get`] returns.
    pub fn get(&self) -> Ptr<T> {
        Ptr { raw: self.value.get() }
    }

    /// Calls `f` with a raw pointer to the value, for a read through it. This is loom's
    /// `UnsafeCell::with`.
    pub fn with<R>(&self, f: impl FnOnce(*const T) -> R) -> R {
        f(self.value.get())
    }

    /// Calls `f` with a raw pointer to the value, for a write through it. This is loom's
    /// `UnsafeCell::with_mut`.
    pub fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        f(self.value.get())
    }

    /// A mutable reference to the value, as [`std::cell::UnsafeCell::get_mut`] returns.
    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }

    /// Unwraps the value.
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }
}

impl<T: Default> Default for UnsafeCell<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> fmt::Debug for UnsafeCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnsafeCell").finish_non_exhaustive()
    }
}

/// A raw pointer into an [`UnsafeCell`]. It is `Copy` and neither `Send` nor `Sync`, as a raw
/// pointer is.
pub struct Ptr<T> {
    raw: *mut T,
}

impl<T> Clone for Ptr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Ptr<T> {}

impl<T> fmt::Debug for Ptr<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ptr({:?})", self.raw)
    }
}

impl<T> Ptr<T> {
    /// The plain raw pointer. Shuttle doesn't see the accesses through it.
    pub fn as_raw(self) -> *mut T {
        self.raw
    }

    /// The same pointer, to a `U`.
    pub fn cast<U>(self) -> Ptr<U> {
        Ptr { raw: self.raw.cast() }
    }

    /// A pointer to a part of the value, such as a field. `f` gets the raw pointer and returns one
    /// into the same value, as `&raw mut (*p).field` does. That makes no reference.
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
        Ptr { raw: f(self.raw) }
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
        }
    }

    /// Reads the value, as [`pointer::read`] does.
    ///
    /// # Safety
    ///
    /// As for [`pointer::read`].
    ///
    /// [`pointer::read`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.read
    pub unsafe fn read(self) -> T {
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
    pub unsafe fn write(self, value: T) {
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
    pub unsafe fn replace(self, value: T) -> T {
        // SAFETY: the caller's.
        unsafe { self.raw.replace(value) }
    }

    /// Makes a shared reference, as `&*ptr` does.
    ///
    /// # Safety
    ///
    /// As for `&*ptr`.
    pub unsafe fn as_ref<'a>(self) -> PtrRef<'a, T> {
        PtrRef {
            raw: self.raw,
            _marker: PhantomData,
        }
    }

    /// Makes a mutable reference, as `&mut *ptr` does.
    ///
    /// # Safety
    ///
    /// As for `&mut *ptr`.
    pub unsafe fn as_mut<'a>(self) -> PtrRefMut<'a, T> {
        PtrRefMut {
            raw: self.raw,
            _marker: PhantomData,
        }
    }
}

/// A shared reference into an [`UnsafeCell`], which [`Ptr::as_ref`] makes.
pub struct PtrRef<'a, T> {
    raw: *const T,
    _marker: PhantomData<&'a T>,
}

impl<T> PtrRef<'_, T> {
    /// A raw pointer from this reference, as `r as *const T` gives. Writing through it is UB, as it
    /// is for the raw pointer.
    pub fn as_ptr(this: &Self) -> Ptr<T> {
        Ptr {
            raw: this.raw.cast_mut(),
        }
    }
}

impl<T> Deref for PtrRef<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: `Ptr::as_ref`'s caller promised that this is a valid reference.
        unsafe { &*self.raw }
    }
}

impl<T: fmt::Debug> fmt::Debug for PtrRef<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

/// A mutable reference into an [`UnsafeCell`], which [`Ptr::as_mut`] makes.
pub struct PtrRefMut<'a, T> {
    raw: *mut T,
    _marker: PhantomData<&'a mut T>,
}

impl<T> PtrRefMut<'_, T> {
    /// A raw pointer from this reference, as `r as *mut T` gives.
    pub fn as_ptr(this: &mut Self) -> Ptr<T> {
        Ptr { raw: this.raw }
    }
}

impl<T> Deref for PtrRefMut<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: `Ptr::as_mut`'s caller promised that this is a valid reference.
        unsafe { &*self.raw }
    }
}

impl<T> DerefMut for PtrRefMut<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: `Ptr::as_mut`'s caller promised that this is a valid reference.
        unsafe { &mut *self.raw }
    }
}

impl<T: fmt::Debug> fmt::Debug for PtrRefMut<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}
