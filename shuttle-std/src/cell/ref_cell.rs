use super::UnsafeCell;
use std::cell::{BorrowError, BorrowMutError};
use std::cmp::Ordering;
use std::fmt;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

/// A version of [`std::cell::RefCell`] that keeps its borrow state in an [`UnsafeCell`], so that
/// each borrow, and the end of each borrow, goes through a [`Ptr`](super::Ptr).
///
/// Each access to the value happens while a borrow holds it, so the accesses to the borrow state
/// order the accesses to the value. Unlike std's `RefCell`, `new` isn't `const`.
pub struct RefCell<T: ?Sized> {
    borrow: UnsafeCell<BorrowFlag>,
    value: std::cell::UnsafeCell<T>,
}

/// A positive value counts the `Ref`s, and a negative value the `RefMut`s. There is more than one
/// `RefMut` only after [`RefMut::map_split`].
type BorrowFlag = isize;

const UNUSED: BorrowFlag = 0;

fn is_writing(flag: BorrowFlag) -> bool {
    flag < UNUSED
}

fn is_reading(flag: BorrowFlag) -> bool {
    flag > UNUSED
}

fn get_flag(borrow: &UnsafeCell<BorrowFlag>) -> BorrowFlag {
    // SAFETY: a `RefCell` isn't `Sync`, and it lends out no reference to its borrow state.
    unsafe { borrow.get().read() }
}

fn set_flag(borrow: &UnsafeCell<BorrowFlag>, flag: BorrowFlag) {
    // SAFETY: as in `get_flag`.
    unsafe { borrow.get().write(flag) }
}

/// std's `BorrowError` has no public constructor, so this gets one from a std `RefCell`.
fn borrow_error() -> BorrowError {
    let cell = std::cell::RefCell::new(());
    let _guard = cell.borrow_mut();
    let error = cell.try_borrow().err();
    error.expect("a mutably borrowed RefCell can't be borrowed")
}

/// std's `BorrowMutError` has no public constructor, so this gets one from a std `RefCell`.
fn borrow_mut_error() -> BorrowMutError {
    let cell = std::cell::RefCell::new(());
    let _guard = cell.borrow();
    let error = cell.try_borrow_mut().err();
    error.expect("a borrowed RefCell can't be borrowed mutably")
}

impl<T> RefCell<T> {
    /// Makes a new `RefCell` holding `value`.
    pub fn new(value: T) -> Self {
        Self {
            borrow: UnsafeCell::new(UNUSED),
            value: std::cell::UnsafeCell::new(value),
        }
    }

    /// Unwraps the value.
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    /// Replaces the value, and returns the old one.
    ///
    /// # Panics
    ///
    /// Panics if the value is borrowed.
    #[track_caller]
    pub fn replace(&self, value: T) -> T {
        std::mem::replace(&mut *self.borrow_mut(), value)
    }

    /// Replaces the value with the one that `f` computes from it, and returns the old one.
    ///
    /// # Panics
    ///
    /// Panics if the value is borrowed.
    #[track_caller]
    pub fn replace_with<F: FnOnce(&mut T) -> T>(&self, f: F) -> T {
        let mut borrow = self.borrow_mut();
        let replacement = f(&mut borrow);
        std::mem::replace(&mut *borrow, replacement)
    }

    /// Swaps the values of two `RefCell`s.
    ///
    /// # Panics
    ///
    /// Panics if either value is borrowed, or if `self` and `other` are the same `RefCell`.
    #[track_caller]
    pub fn swap(&self, other: &Self) {
        std::mem::swap(&mut *self.borrow_mut(), &mut *other.borrow_mut())
    }
}

impl<T: ?Sized> RefCell<T> {
    /// Borrows the value, as [`std::cell::RefCell::borrow`] does.
    ///
    /// # Panics
    ///
    /// Panics if the value is mutably borrowed.
    #[track_caller]
    pub fn borrow(&self) -> Ref<'_, T> {
        match self.try_borrow() {
            Ok(borrow) => borrow,
            Err(error) => panic!("{error}"),
        }
    }

    /// Borrows the value, or fails if it is mutably borrowed.
    pub fn try_borrow(&self) -> Result<Ref<'_, T>, BorrowError> {
        match BorrowRef::new(&self.borrow) {
            Some(borrow) => Ok(Ref {
                // SAFETY: `UnsafeCell::get` doesn't return a null pointer.
                value: unsafe { NonNull::new_unchecked(self.value.get()) },
                borrow,
            }),
            None => Err(borrow_error()),
        }
    }

    /// Borrows the value mutably, as [`std::cell::RefCell::borrow_mut`] does.
    ///
    /// # Panics
    ///
    /// Panics if the value is borrowed.
    #[track_caller]
    pub fn borrow_mut(&self) -> RefMut<'_, T> {
        match self.try_borrow_mut() {
            Ok(borrow) => borrow,
            Err(error) => panic!("{error}"),
        }
    }

    /// Borrows the value mutably, or fails if it is borrowed.
    pub fn try_borrow_mut(&self) -> Result<RefMut<'_, T>, BorrowMutError> {
        match BorrowRefMut::new(&self.borrow) {
            Some(borrow) => Ok(RefMut {
                // SAFETY: `UnsafeCell::get` doesn't return a null pointer.
                value: unsafe { NonNull::new_unchecked(self.value.get()) },
                borrow,
                marker: PhantomData,
            }),
            None => Err(borrow_mut_error()),
        }
    }

    /// A raw pointer to the value. Shuttle doesn't see the accesses through it.
    pub fn as_ptr(&self) -> *mut T {
        self.value.get()
    }

    /// A mutable reference to the value.
    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }

    /// A reference to the value, without a borrow that lasts while it lives, or an error if the
    /// value is mutably borrowed now.
    ///
    /// # Safety
    ///
    /// As for [`std::cell::RefCell::try_borrow_unguarded`]: the value must not be mutably borrowed
    /// while the reference lives.
    pub unsafe fn try_borrow_unguarded(&self) -> Result<&T, BorrowError> {
        if is_writing(get_flag(&self.borrow)) {
            Err(borrow_error())
        } else {
            // SAFETY: no `RefMut` exists now, and the caller promises that none is made while the
            // reference lives.
            Ok(unsafe { &*self.value.get() })
        }
    }
}

impl<T: Default> RefCell<T> {
    /// Takes the value, and leaves `T::default()` in its place.
    ///
    /// # Panics
    ///
    /// Panics if the value is borrowed.
    #[track_caller]
    pub fn take(&self) -> T {
        self.replace(T::default())
    }
}

impl<T: Clone> Clone for RefCell<T> {
    /// # Panics
    ///
    /// Panics if the value is mutably borrowed.
    #[track_caller]
    fn clone(&self) -> Self {
        Self::new(self.borrow().clone())
    }

    /// # Panics
    ///
    /// Panics if `source`'s value is mutably borrowed.
    #[track_caller]
    fn clone_from(&mut self, source: &Self) {
        self.get_mut().clone_from(&source.borrow())
    }
}

impl<T: Default> Default for RefCell<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: ?Sized + PartialEq> PartialEq for RefCell<T> {
    fn eq(&self, other: &Self) -> bool {
        *self.borrow() == *other.borrow()
    }
}

impl<T: ?Sized + Eq> Eq for RefCell<T> {}

impl<T: ?Sized + PartialOrd> PartialOrd for RefCell<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.borrow().partial_cmp(&*other.borrow())
    }
}

impl<T: ?Sized + Ord> Ord for RefCell<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.borrow().cmp(&*other.borrow())
    }
}

impl<T> From<T> for RefCell<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RefCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("RefCell");
        match self.try_borrow() {
            Ok(borrow) => d.field("value", &borrow),
            Err(_) => d.field("value", &format_args!("<borrowed>")),
        };
        d.finish()
    }
}

/// One of the `Ref`s of a `RefCell`, counted in its borrow state.
struct BorrowRef<'b> {
    borrow: &'b UnsafeCell<BorrowFlag>,
}

impl<'b> BorrowRef<'b> {
    fn new(borrow: &'b UnsafeCell<BorrowFlag>) -> Option<Self> {
        let flag = get_flag(borrow).wrapping_add(1);
        if !is_reading(flag) {
            // There is a `RefMut`, or there are `isize::MAX` `Ref`s, which only leaking them makes.
            return None;
        }
        set_flag(borrow, flag);
        Some(Self { borrow })
    }
}

impl Clone for BorrowRef<'_> {
    fn clone(&self) -> Self {
        let flag = get_flag(self.borrow);
        debug_assert!(is_reading(flag));
        assert!(flag != BorrowFlag::MAX, "too many `Ref`s of one RefCell");
        set_flag(self.borrow, flag + 1);
        Self { borrow: self.borrow }
    }
}

impl Drop for BorrowRef<'_> {
    fn drop(&mut self) {
        let flag = get_flag(self.borrow);
        debug_assert!(is_reading(flag));
        set_flag(self.borrow, flag - 1);
    }
}

/// One of the `RefMut`s of a `RefCell`, counted in its borrow state.
struct BorrowRefMut<'b> {
    borrow: &'b UnsafeCell<BorrowFlag>,
}

impl<'b> BorrowRefMut<'b> {
    fn new(borrow: &'b UnsafeCell<BorrowFlag>) -> Option<Self> {
        if get_flag(borrow) != UNUSED {
            return None;
        }
        set_flag(borrow, UNUSED - 1);
        Some(Self { borrow })
    }

    /// Another `RefMut`, for a part of the value that the first one doesn't use any more.
    fn split(&self) -> Self {
        let flag = get_flag(self.borrow);
        debug_assert!(is_writing(flag));
        assert!(flag != BorrowFlag::MIN, "too many `RefMut`s of one RefCell");
        set_flag(self.borrow, flag - 1);
        Self { borrow: self.borrow }
    }
}

impl Drop for BorrowRefMut<'_> {
    fn drop(&mut self) {
        let flag = get_flag(self.borrow);
        debug_assert!(is_writing(flag));
        set_flag(self.borrow, flag + 1);
    }
}

/// A borrow of a [`RefCell`]'s value, as [`std::cell::Ref`] is.
pub struct Ref<'b, T: ?Sized + 'b> {
    // Not a `&'b T`: a reference that is passed to a function must stay valid until the function
    // returns, and the function can drop the `Ref`.
    value: NonNull<T>,
    borrow: BorrowRef<'b>,
}

impl<T: ?Sized> Deref for Ref<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the borrow keeps any `RefMut` from being made while the `Ref` lives.
        unsafe { self.value.as_ref() }
    }
}

impl<'b, T: ?Sized> Ref<'b, T> {
    /// Copies a `Ref`, as [`std::cell::Ref::clone`] does. It is a function and not a method, so
    /// that `r.clone()` clones the value.
    #[allow(clippy::should_implement_trait)]
    pub fn clone(orig: &Ref<'b, T>) -> Ref<'b, T> {
        Ref {
            value: orig.value,
            borrow: orig.borrow.clone(),
        }
    }

    /// A `Ref` of a part of the value, as [`std::cell::Ref::map`] makes.
    pub fn map<U: ?Sized, F>(orig: Ref<'b, T>, f: F) -> Ref<'b, U>
    where
        F: FnOnce(&T) -> &U,
    {
        Ref {
            value: NonNull::from(f(&*orig)),
            borrow: orig.borrow,
        }
    }

    /// A `Ref` of a part of the value, or the original `Ref` if `f` returns `None`, as
    /// [`std::cell::Ref::filter_map`] makes.
    pub fn filter_map<U: ?Sized, F>(orig: Ref<'b, T>, f: F) -> Result<Ref<'b, U>, Self>
    where
        F: FnOnce(&T) -> Option<&U>,
    {
        match f(&*orig) {
            Some(value) => Ok(Ref {
                value: NonNull::from(value),
                borrow: orig.borrow,
            }),
            None => Err(orig),
        }
    }

    /// Two `Ref`s of two parts of the value, as [`std::cell::Ref::map_split`] makes.
    pub fn map_split<U: ?Sized, V: ?Sized, F>(orig: Ref<'b, T>, f: F) -> (Ref<'b, U>, Ref<'b, V>)
    where
        F: FnOnce(&T) -> (&U, &V),
    {
        let (a, b) = f(&*orig);
        let borrow = orig.borrow.clone();
        (
            Ref {
                value: NonNull::from(a),
                borrow,
            },
            Ref {
                value: NonNull::from(b),
                borrow: orig.borrow,
            },
        )
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Ref<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized + fmt::Display> fmt::Display for Ref<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

/// A mutable borrow of a [`RefCell`]'s value, as [`std::cell::RefMut`] is.
pub struct RefMut<'b, T: ?Sized + 'b> {
    // Not a `&'b mut T`, for the reason in `Ref`.
    value: NonNull<T>,
    borrow: BorrowRefMut<'b>,
    // `NonNull` is covariant in `T`, and a `RefMut` must be invariant, as `&mut T` is.
    marker: PhantomData<&'b mut T>,
}

impl<T: ?Sized> Deref for RefMut<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the borrow keeps any other `Ref` or `RefMut` of this part of the value from being
        // made while the `RefMut` lives.
        unsafe { self.value.as_ref() }
    }
}

impl<T: ?Sized> DerefMut for RefMut<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`.
        unsafe { self.value.as_mut() }
    }
}

impl<'b, T: ?Sized> RefMut<'b, T> {
    /// A `RefMut` of a part of the value, as [`std::cell::RefMut::map`] makes.
    pub fn map<U: ?Sized, F>(mut orig: RefMut<'b, T>, f: F) -> RefMut<'b, U>
    where
        F: FnOnce(&mut T) -> &mut U,
    {
        let value = NonNull::from(f(&mut *orig));
        RefMut {
            value,
            borrow: orig.borrow,
            marker: PhantomData,
        }
    }

    /// A `RefMut` of a part of the value, or the original `RefMut` if `f` returns `None`, as
    /// [`std::cell::RefMut::filter_map`] makes.
    pub fn filter_map<U: ?Sized, F>(mut orig: RefMut<'b, T>, f: F) -> Result<RefMut<'b, U>, Self>
    where
        F: FnOnce(&mut T) -> Option<&mut U>,
    {
        match f(&mut *orig) {
            Some(value) => Ok(RefMut {
                value: NonNull::from(value),
                borrow: orig.borrow,
                marker: PhantomData,
            }),
            None => Err(orig),
        }
    }

    /// Two `RefMut`s of two parts of the value, as [`std::cell::RefMut::map_split`] makes.
    pub fn map_split<U: ?Sized, V: ?Sized, F>(mut orig: RefMut<'b, T>, f: F) -> (RefMut<'b, U>, RefMut<'b, V>)
    where
        F: FnOnce(&mut T) -> (&mut U, &mut V),
    {
        let borrow = orig.borrow.split();
        let (a, b) = f(&mut *orig);
        (
            RefMut {
                value: NonNull::from(a),
                borrow,
                marker: PhantomData,
            },
            RefMut {
                value: NonNull::from(b),
                borrow: orig.borrow,
                marker: PhantomData,
            },
        )
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RefMut<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized + fmt::Display> fmt::Display for RefMut<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}
