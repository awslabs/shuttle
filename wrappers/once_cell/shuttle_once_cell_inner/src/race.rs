//! Thread-safe, non-blocking, "first one wins" flavor of `OnceCell`: Shuttle's models of
//! `once_cell::race`.
//!
//! If two tasks race to initialize a type from the `race` module, they don't block, execute
//! initialization function together, but only one of them stores the result.
//!
//! # Shuttle
//!
//! Each type keeps its value in the running execution's storage, by way of Shuttle's
//! `std::sync::OnceLock` model, so a `static` cell is empty again in every execution.
//!
//! A task that finds the cell empty runs its initializer outside of any lock, as in once_cell, and
//! then stores the result unless another task has stored one meanwhile. Storing it takes the two
//! scheduling points of `OnceLock::set` on an empty cell, where once_cell does a compare-exchange,
//! so Shuttle explores the schedules in which several tasks run their initializers as well as those
//! in which only one does, and all of them return the value that won. Reading a cell that has a
//! value is not a scheduling point, and it takes causality from the task that stored the value,
//! as the `Acquire` load in once_cell synchronizes with the `Release` that stored it.

use shuttle::sync::OnceLock;
use std::cell::UnsafeCell;
use std::fmt;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::ptr::{self, NonNull};

/// A thread-safe cell which can be written to only once.
#[derive(Default)]
pub struct OnceNonZeroUsize {
    cell: OnceLock<NonZeroUsize>,
}

impl OnceNonZeroUsize {
    /// Creates a new empty cell.
    #[inline]
    #[must_use]
    #[track_caller]
    pub const fn new() -> Self {
        Self { cell: OnceLock::new() }
    }

    /// Gets the underlying value.
    #[inline]
    pub fn get(&self) -> Option<NonZeroUsize> {
        self.cell.get().copied()
    }

    /// Get the reference to the underlying value, without checking if the cell is initialized.
    ///
    /// # Safety
    ///
    /// Caller must ensure that the cell is in initialized state, and that the contents are
    /// acquired by (synchronized to) this thread.
    ///
    /// # Panics
    ///
    /// Under Shuttle the check is made anyway, and this panics if the cell is empty, where
    /// once_cell's behavior is undefined.
    pub unsafe fn get_unchecked(&self) -> NonZeroUsize {
        self.get().expect(
            "`OnceNonZeroUsize::get_unchecked` was called on an empty cell, which is undefined behavior in once_cell",
        )
    }

    /// Sets the contents of this cell to `value`.
    ///
    /// Returns `Ok(())` if the cell was empty and `Err(())` if it was full.
    #[inline]
    #[allow(clippy::result_unit_err)]
    pub fn set(&self, value: NonZeroUsize) -> Result<(), ()> {
        self.cell.set(value).map_err(|_| ())
    }

    /// Gets the contents of the cell, initializing it with `f` if the cell was empty.
    ///
    /// If several tasks concurrently run `get_or_init`, more than one `f` can be called. However,
    /// all tasks will return the same value, produced by some `f`.
    pub fn get_or_init<F>(&self, f: F) -> NonZeroUsize
    where
        F: FnOnce() -> NonZeroUsize,
    {
        enum Void {}
        match self.get_or_try_init(|| Ok::<NonZeroUsize, Void>(f())) {
            Ok(val) => val,
            Err(void) => match void {},
        }
    }

    /// Gets the contents of the cell, initializing it with `f` if the cell was empty. If the cell
    /// was empty and `f` failed, an error is returned.
    ///
    /// If several tasks concurrently run `get_or_init`, more than one `f` can be called. However,
    /// all tasks will return the same value, produced by some `f`.
    pub fn get_or_try_init<F, E>(&self, f: F) -> Result<NonZeroUsize, E>
    where
        F: FnOnce() -> Result<NonZeroUsize, E>,
    {
        get_or_try_init_racy(&self.cell, f).copied()
    }
}

impl fmt::Debug for OnceNonZeroUsize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // As once_cell's derived impl, which prints the integer it stores, 0 for an empty cell.
        let inner = self.get().map_or(0, NonZeroUsize::get);
        f.debug_struct("OnceNonZeroUsize").field("inner", &inner).finish()
    }
}

/// A thread-safe cell which can be written to only once.
#[derive(Default, Debug)]
pub struct OnceBool {
    inner: OnceNonZeroUsize,
}

impl OnceBool {
    /// Creates a new empty cell.
    #[inline]
    #[must_use]
    #[track_caller]
    pub const fn new() -> Self {
        Self {
            inner: OnceNonZeroUsize::new(),
        }
    }

    /// Gets the underlying value.
    #[inline]
    pub fn get(&self) -> Option<bool> {
        self.inner.get().map(Self::from_usize)
    }

    /// Sets the contents of this cell to `value`.
    ///
    /// Returns `Ok(())` if the cell was empty and `Err(())` if it was full.
    #[inline]
    #[allow(clippy::result_unit_err)]
    pub fn set(&self, value: bool) -> Result<(), ()> {
        self.inner.set(Self::to_usize(value))
    }

    /// Gets the contents of the cell, initializing it with `f` if the cell was empty.
    ///
    /// If several tasks concurrently run `get_or_init`, more than one `f` can be called. However,
    /// all tasks will return the same value, produced by some `f`.
    pub fn get_or_init<F>(&self, f: F) -> bool
    where
        F: FnOnce() -> bool,
    {
        Self::from_usize(self.inner.get_or_init(|| Self::to_usize(f())))
    }

    /// Gets the contents of the cell, initializing it with `f` if the cell was empty. If the cell
    /// was empty and `f` failed, an error is returned.
    ///
    /// If several tasks concurrently run `get_or_init`, more than one `f` can be called. However,
    /// all tasks will return the same value, produced by some `f`.
    pub fn get_or_try_init<F, E>(&self, f: F) -> Result<bool, E>
    where
        F: FnOnce() -> Result<bool, E>,
    {
        self.inner
            .get_or_try_init(|| f().map(Self::to_usize))
            .map(Self::from_usize)
    }

    #[inline]
    fn from_usize(value: NonZeroUsize) -> bool {
        value.get() == 1
    }

    #[inline]
    fn to_usize(value: bool) -> NonZeroUsize {
        NonZeroUsize::new(if value { 1 } else { 2 }).unwrap()
    }
}

/// A thread-safe cell which can be written to only once.
pub struct OnceRef<'a, T> {
    /// The reference, as a pointer, so that the storage of the execution can hold it whatever `'a`
    /// is.
    cell: OnceLock<Pointer>,
    /// As once_cell's: `OnceRef` is invariant in `'a` and `T`, so that two cells whose references
    /// live for different lifetimes cannot be swapped, and it has once_cell's auto traits.
    ghost: PhantomData<UnsafeCell<&'a T>>,
}

/// The address a `OnceRef` stores.
#[derive(Clone, Copy)]
struct Pointer(NonNull<()>);

// Safety: a `Pointer` is only an address; what it points to is shared, and sent, as the `&'a T` the
// `OnceRef` hands out, which its own `Sync` bound covers.
unsafe impl Send for Pointer {}
unsafe impl Sync for Pointer {}

// Safety: once_cell's bound. Sharing the cell shares the `&'a T`s it hands out, which needs
// `T: Sync`.
unsafe impl<T: Sync> Sync for OnceRef<'_, T> {}

impl<T> fmt::Debug for OnceRef<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // As once_cell's, which prints the pointer it stores.
        let pointer = self
            .cell
            .get()
            .map_or(ptr::null_mut(), |pointer| pointer.0.as_ptr().cast::<T>());
        write!(f, "OnceRef({pointer:?})")
    }
}

impl<T> Default for OnceRef<'_, T> {
    #[track_caller]
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, T> OnceRef<'a, T> {
    /// Creates a new empty cell.
    #[must_use]
    #[track_caller]
    pub const fn new() -> Self {
        Self {
            cell: OnceLock::new(),
            ghost: PhantomData,
        }
    }

    /// Gets a reference to the underlying value.
    pub fn get(&self) -> Option<&'a T> {
        self.cell.get().map(|pointer| Self::from_pointer(*pointer))
    }

    /// Sets the contents of this cell to `value`.
    ///
    /// Returns `Ok(())` if the cell was empty and `Err(())` if it was full.
    #[allow(clippy::result_unit_err)]
    pub fn set(&self, value: &'a T) -> Result<(), ()> {
        self.cell.set(Self::to_pointer(value)).map_err(|_| ())
    }

    /// Gets the contents of the cell, initializing it with `f` if the cell was empty.
    ///
    /// If several tasks concurrently run `get_or_init`, more than one `f` can be called. However,
    /// all tasks will return the same value, produced by some `f`.
    pub fn get_or_init<F>(&self, f: F) -> &'a T
    where
        F: FnOnce() -> &'a T,
    {
        enum Void {}
        match self.get_or_try_init(|| Ok::<&'a T, Void>(f())) {
            Ok(val) => val,
            Err(void) => match void {},
        }
    }

    /// Gets the contents of the cell, initializing it with `f` if the cell was empty. If the cell
    /// was empty and `f` failed, an error is returned.
    ///
    /// If several tasks concurrently run `get_or_init`, more than one `f` can be called. However,
    /// all tasks will return the same value, produced by some `f`.
    pub fn get_or_try_init<F, E>(&self, f: F) -> Result<&'a T, E>
    where
        F: FnOnce() -> Result<&'a T, E>,
    {
        get_or_try_init_racy(&self.cell, || f().map(Self::to_pointer)).map(|pointer| Self::from_pointer(*pointer))
    }

    fn to_pointer(value: &'a T) -> Pointer {
        Pointer(NonNull::from(value).cast())
    }

    fn from_pointer(pointer: Pointer) -> &'a T {
        // Safety: every pointer in the cell came from a `&'a T` passed to this cell, and the cell
        // is invariant in `'a`, so the reference it was made from is still alive for `'a`. The
        // storage of an execution holds the pointer at most until the execution ends, and only
        // this cell, which keys it, can read it.
        unsafe { pointer.0.cast::<T>().as_ref() }
    }

    /// ```compile_fail
    /// use shuttle_once_cell_inner::race::OnceRef;
    ///
    /// let mut l = OnceRef::new();
    ///
    /// {
    ///     let y = 2;
    ///     let mut r = OnceRef::new();
    ///     r.set(&y).unwrap();
    ///     core::mem::swap(&mut l, &mut r);
    /// }
    ///
    /// // l now contains a dangling reference to y
    /// eprintln!("uaf: {}", l.get().unwrap());
    /// ```
    fn _dummy() {}
}

#[cfg(feature = "alloc")]
pub use self::once_box::OnceBox;

#[cfg(feature = "alloc")]
mod once_box {
    use super::get_or_try_init_racy;
    use shuttle::sync::OnceLock;
    use std::fmt;
    use std::panic::{RefUnwindSafe, UnwindSafe};
    use std::ptr;

    /// A thread-safe cell which can be written to only once.
    pub struct OnceBox<T> {
        /// The value the cell was created with, which is the same in every execution.
        initial: Option<Box<T>>,
        /// The value stored in the running execution, if `initial` is `None`.
        cell: OnceLock<Box<T>>,
    }

    // once_cell's auto traits. Its `OnceBox` is an `AtomicPtr<T>`, which is `RefUnwindSafe` for
    // any `T` and `UnwindSafe` for a `RefUnwindSafe` one, beside a `PhantomData<Option<Box<T>>>`.
    impl<T: RefUnwindSafe + UnwindSafe> UnwindSafe for OnceBox<T> {}
    impl<T: RefUnwindSafe> RefUnwindSafe for OnceBox<T> {}

    impl<T: 'static> fmt::Debug for OnceBox<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            // As once_cell's, which prints the pointer it stores.
            let pointer = match &self.initial {
                Some(value) => &**value as *const T,
                None => self.cell.get().map_or(ptr::null(), |value| &**value as *const T),
            };
            write!(f, "OnceBox({pointer:?})")
        }
    }

    impl<T> Default for OnceBox<T> {
        #[track_caller]
        fn default() -> Self {
            Self::new()
        }
    }

    impl<T> OnceBox<T> {
        /// Creates a new empty cell.
        #[must_use]
        #[track_caller]
        pub const fn new() -> Self {
            Self {
                initial: None,
                cell: OnceLock::new(),
            }
        }

        /// Creates a new cell with the given value.
        ///
        /// The cell holds `value` itself, so it has the value in every execution, and it can be
        /// read outside of one too.
        #[must_use]
        #[track_caller]
        pub fn with_value(value: Box<T>) -> Self {
            Self {
                initial: Some(value),
                cell: OnceLock::new(),
            }
        }
    }

    impl<T: 'static> OnceBox<T> {
        /// Gets a reference to the underlying value.
        pub fn get(&self) -> Option<&T> {
            match &self.initial {
                Some(value) => Some(value),
                None => self.cell.get().map(|value| &**value),
            }
        }

        /// Sets the contents of this cell to `value`.
        ///
        /// Returns `Ok(())` if the cell was empty and `Err(value)` if it was full.
        pub fn set(&self, value: Box<T>) -> Result<(), Box<T>> {
            match &self.initial {
                Some(_) => Err(value),
                None => self.cell.set(value),
            }
        }

        /// Gets the contents of the cell, initializing it with `f` if the cell was empty.
        ///
        /// If several tasks concurrently run `get_or_init`, more than one `f` can be called.
        /// However, all tasks will return the same value, produced by some `f`.
        pub fn get_or_init<F>(&self, f: F) -> &T
        where
            F: FnOnce() -> Box<T>,
        {
            enum Void {}
            match self.get_or_try_init(|| Ok::<Box<T>, Void>(f())) {
                Ok(val) => val,
                Err(void) => match void {},
            }
        }

        /// Gets the contents of the cell, initializing it with `f` if the cell was empty. If the
        /// cell was empty and `f` failed, an error is returned.
        ///
        /// If several tasks concurrently run `get_or_init`, more than one `f` can be called.
        /// However, all tasks will return the same value, produced by some `f`.
        pub fn get_or_try_init<F, E>(&self, f: F) -> Result<&T, E>
        where
            F: FnOnce() -> Result<Box<T>, E>,
        {
            match &self.initial {
                Some(value) => Ok(value),
                None => get_or_try_init_racy(&self.cell, f).map(|value| &**value),
            }
        }
    }

    impl<T: Clone + 'static> Clone for OnceBox<T> {
        #[track_caller]
        fn clone(&self) -> Self {
            match self.get() {
                Some(value) => OnceBox::with_value(Box::new(value.clone())),
                None => OnceBox::new(),
            }
        }
    }

    /// ```compile_fail
    /// struct S(*mut ());
    /// unsafe impl Sync for S {}
    ///
    /// fn share<T: Sync>(_: &T) {}
    /// share(&shuttle_once_cell_inner::race::OnceBox::<S>::new());
    /// ```
    fn _dummy() {}
}

/// The `race` flavor of initialization: returns the cell's value if it has one; otherwise runs `f`
/// outside of any lock, and stores its value unless another task stored one meanwhile, in which
/// case that one wins and `f`'s is dropped.
///
/// Not a scheduling point if the cell has a value. Otherwise storing the value takes the two
/// scheduling points of `OnceLock::set` on an empty cell, after `f` has run.
fn get_or_try_init_racy<T: 'static, E>(cell: &OnceLock<T>, f: impl FnOnce() -> Result<T, E>) -> Result<&T, E> {
    if let Some(value) = cell.get() {
        return Ok(value);
    }
    let value = f()?;
    Ok(match cell.try_insert(value) {
        Ok(ours) => ours,
        Err((theirs, _ours)) => theirs,
    })
}
