//! Comparing the things that can stand for a stored pointer.

use crate::Guard;
use crate::ref_cnt::RefCnt;

mod sealed {
    pub trait Sealed {}
}
use sealed::Sealed;

/// A trait describing things that can be turned into a raw pointer.
///
/// This is just an abstraction of things that can be passed to the
/// [`compare_and_swap`](crate::ArcSwapAny::compare_and_swap) method.
pub trait AsRaw<T>: Sealed {
    /// Converts the value into a raw pointer.
    fn as_raw(&self) -> *mut T;
}

impl<T: RefCnt> Sealed for &T {}
impl<T: RefCnt> AsRaw<T::Base> for &T {
    fn as_raw(&self) -> *mut T::Base {
        T::as_ptr(self)
    }
}

impl<T: RefCnt> Sealed for &Guard<T> {}
impl<T: RefCnt> AsRaw<T::Base> for &Guard<T> {
    fn as_raw(&self) -> *mut T::Base {
        T::as_ptr(self)
    }
}

impl<T: RefCnt> Sealed for Guard<T> {}
impl<T: RefCnt> AsRaw<T::Base> for Guard<T> {
    fn as_raw(&self) -> *mut T::Base {
        T::as_ptr(self)
    }
}

impl<T> Sealed for *mut T {}
impl<T> AsRaw<T> for *mut T {
    fn as_raw(&self) -> *mut T {
        *self
    }
}

impl<T> Sealed for *const T {}
impl<T> AsRaw<T> for *const T {
    fn as_raw(&self) -> *mut T {
        *self as *mut T
    }
}
