//! The smart pointers an [`ArcSwapAny`](crate::ArcSwapAny) can hold.

use std::pin::Pin;
use std::ptr;
use std::rc::Rc;
use std::sync::Arc;

/// A trait describing smart reference counted pointers.
///
/// Note that in a distant future, this might be replaced by stdlib `std::ops::Receiver` or
/// something similar.
///
/// The trait is `unsafe` to implement, as the implementation has to be correct: the pointers have
/// to be created from and turned back into the pointers the smart pointer type owns, without
/// losing or duplicating a reference count. The implementations for `Arc`, `Rc`, `Option<T>` and
/// `Pin` of those are provided here; this trait is the same as
/// [`arc_swap::RefCnt`](https://docs.rs/arc-swap/1/arc_swap/trait.RefCnt.html), so an
/// implementation written for arc-swap works here too.
///
/// # Safety
///
/// Implementations must ensure that:
///
/// * [`into_ptr`](RefCnt::into_ptr) gives up one reference count, which
///   [`from_ptr`](RefCnt::from_ptr) takes back.
/// * [`as_ptr`](RefCnt::as_ptr) borrows the pointer without changing any count, and returns the
///   same address for the same value.
/// * A `Self` created by `from_ptr` from an `into_ptr` of a value is that value.
pub unsafe trait RefCnt: Clone {
    /// The base type the pointer points to.
    type Base;

    /// Converts the smart pointer into a raw pointer, without affecting the reference count.
    ///
    /// This can be seen as kind of freezing the pointer, so that it can be stored somewhere, and
    /// later turned back with [`from_ptr`](RefCnt::from_ptr).
    fn into_ptr(me: Self) -> *mut Self::Base;

    /// Provides a view into the smart pointer as a raw pointer.
    ///
    /// This must not change any reference counts, and the pointer must stay valid for as long as
    /// `me` is alive.
    fn as_ptr(me: &Self) -> *mut Self::Base;

    /// Converts a raw pointer back into the smart pointer, without affecting the reference count.
    ///
    /// # Safety
    ///
    /// The pointer must have come from [`into_ptr`](RefCnt::into_ptr) or
    /// [`as_ptr`](RefCnt::as_ptr), and the reference count it stands for must not have been given
    /// to another `Self`.
    unsafe fn from_ptr(ptr: *const Self::Base) -> Self;

    /// Increments the reference count by one, and returns a raw pointer that owns it.
    fn inc(me: &Self) -> *mut Self::Base {
        Self::into_ptr(Self::clone(me))
    }

    /// Decrements the reference count that `ptr` owns.
    ///
    /// # Safety
    ///
    /// As [`from_ptr`](RefCnt::from_ptr)'s.
    unsafe fn dec(ptr: *const Self::Base) {
        // Safety: the caller's.
        drop(unsafe { Self::from_ptr(ptr) });
    }
}

// Safety: `Arc`'s raw pointers are the ones it owns, and `into_raw`/`from_raw` move the count
// without changing it.
unsafe impl<T> RefCnt for Arc<T> {
    type Base = T;

    fn into_ptr(me: Arc<T>) -> *mut T {
        Arc::into_raw(me) as *mut T
    }

    fn as_ptr(me: &Arc<T>) -> *mut T {
        // `Arc::as_ptr` does not touch the count, and the address is stable.
        Arc::as_ptr(me) as *mut T
    }

    unsafe fn from_ptr(ptr: *const T) -> Arc<T> {
        // Safety: the caller's.
        unsafe { Arc::from_raw(ptr) }
    }
}

// Safety: as `Arc`'s. An `Rc` is useless in a concurrent test, but arc-swap has the impl, so code
// that names it keeps compiling.
unsafe impl<T> RefCnt for Rc<T> {
    type Base = T;

    fn into_ptr(me: Rc<T>) -> *mut T {
        Rc::into_raw(me) as *mut T
    }

    fn as_ptr(me: &Rc<T>) -> *mut T {
        Rc::as_ptr(me) as *mut T
    }

    unsafe fn from_ptr(ptr: *const T) -> Rc<T> {
        // Safety: the caller's.
        unsafe { Rc::from_raw(ptr) }
    }
}

// Safety: `None` is the null pointer, which no `T` uses, and `Some` is `T`'s own.
unsafe impl<T: RefCnt> RefCnt for Option<T> {
    type Base = T::Base;

    fn into_ptr(me: Option<T>) -> *mut T::Base {
        me.map(T::into_ptr).unwrap_or_else(ptr::null_mut)
    }

    fn as_ptr(me: &Option<T>) -> *mut T::Base {
        me.as_ref().map(T::as_ptr).unwrap_or_else(ptr::null_mut)
    }

    unsafe fn from_ptr(ptr: *const T::Base) -> Option<T> {
        if ptr.is_null() {
            None
        } else {
            // Safety: the caller's.
            Some(unsafe { T::from_ptr(ptr) })
        }
    }
}

// Safety: a `Pin<Arc<T>>` is an `Arc<T>` that promises not to move its contents, which turning it
// into a pointer and back does not.
unsafe impl<T> RefCnt for Pin<Arc<T>> {
    type Base = T;

    fn into_ptr(me: Pin<Arc<T>>) -> *mut T {
        // Safety: the pointer comes from a pinned `Arc`, and goes back into one.
        unsafe { Arc::into_raw(Pin::into_inner_unchecked(me)) as *mut T }
    }

    fn as_ptr(me: &Pin<Arc<T>>) -> *mut T {
        // Safety: as above; this only borrows.
        unsafe { Arc::as_ptr(&*(me as *const Pin<Arc<T>> as *const Arc<T>)) as *mut T }
    }

    unsafe fn from_ptr(ptr: *const T) -> Self {
        // Safety: the caller's; the value was pinned when it was turned into a pointer.
        unsafe { Pin::new_unchecked(Arc::from_raw(ptr)) }
    }
}

// Safety: as `Pin<Arc<T>>`'s.
unsafe impl<T> RefCnt for Pin<Rc<T>> {
    type Base = T;

    fn into_ptr(me: Pin<Rc<T>>) -> *mut T {
        // Safety: as `Pin<Arc<T>>`'s.
        unsafe { Rc::into_raw(Pin::into_inner_unchecked(me)) as *mut T }
    }

    fn as_ptr(me: &Pin<Rc<T>>) -> *mut T {
        // Safety: as `Pin<Arc<T>>`'s.
        unsafe { Rc::as_ptr(&*(me as *const Pin<Rc<T>> as *const Rc<T>)) as *mut T }
    }

    unsafe fn from_ptr(ptr: *const T) -> Self {
        // Safety: the caller's.
        unsafe { Pin::new_unchecked(Rc::from_raw(ptr)) }
    }
}

#[cfg(feature = "weak")]
mod weak {
    use super::RefCnt;
    use std::ptr;
    use std::rc::Weak as RcWeak;
    use std::sync::Weak;

    // Safety: a dangling `Weak` (the one `Weak::new` makes) is the null pointer here, and every
    // other `Weak` owns its weak count, which `into_raw`/`from_raw` move without changing.
    unsafe impl<T> RefCnt for Weak<T> {
        type Base = T;

        fn as_ptr(me: &Self) -> *mut T {
            if Weak::ptr_eq(&Weak::new(), me) {
                ptr::null_mut()
            } else {
                Weak::as_ptr(me) as *mut T
            }
        }

        fn into_ptr(me: Self) -> *mut T {
            if Weak::ptr_eq(&Weak::new(), &me) {
                ptr::null_mut()
            } else {
                Weak::into_raw(me) as *mut T
            }
        }

        unsafe fn from_ptr(ptr: *const T) -> Self {
            if ptr.is_null() {
                Weak::new()
            } else {
                // Safety: the caller's.
                unsafe { Weak::from_raw(ptr) }
            }
        }
    }

    // Safety: as `Weak`'s.
    unsafe impl<T> RefCnt for RcWeak<T> {
        type Base = T;

        fn as_ptr(me: &Self) -> *mut T {
            if RcWeak::ptr_eq(&RcWeak::new(), me) {
                ptr::null_mut()
            } else {
                RcWeak::as_ptr(me) as *mut T
            }
        }

        fn into_ptr(me: Self) -> *mut T {
            if RcWeak::ptr_eq(&RcWeak::new(), &me) {
                ptr::null_mut()
            } else {
                RcWeak::into_raw(me) as *mut T
            }
        }

        unsafe fn from_ptr(ptr: *const T) -> Self {
            if ptr.is_null() {
                RcWeak::new()
            } else {
                // Safety: the caller's.
                unsafe { RcWeak::from_raw(ptr) }
            }
        }
    }
}
