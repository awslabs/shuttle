//! A value whose type the code holding it does not know, for the parts of a selection that are not
//! generic: a `select!` hands a received message or a rendezvous slot from the selection to the
//! operation that completes it.

/// A boxed value of a type that only the code that made it, and the code that takes it back,
/// know. Dropping it drops the value.
pub(crate) struct Erased {
    ptr: *mut (),
    drop: unsafe fn(*mut ()),
}

unsafe fn drop_boxed<X>(ptr: *mut ()) {
    // Safety: `ptr` came from `Box::<X>::into_raw` in `Erased::new`, and is dropped only once.
    drop(unsafe { Box::from_raw(ptr.cast::<X>()) });
}

impl Erased {
    pub(crate) fn new<X>(value: X) -> Self {
        Self {
            ptr: Box::into_raw(Box::new(value)).cast(),
            drop: drop_boxed::<X>,
        }
    }

    /// Takes the value back.
    ///
    /// # Safety
    ///
    /// `X` must be the type the value was made with.
    pub(crate) unsafe fn take<X>(self) -> X {
        let ptr = self.ptr.cast::<X>();
        std::mem::forget(self);
        // Safety: the caller promises that the value is an `X`, and `forget` stops `Drop` from
        // dropping it a second time.
        *unsafe { Box::from_raw(ptr) }
    }
}

impl Drop for Erased {
    fn drop(&mut self) {
        // Safety: the value has not been taken, as `take` forgets `self`.
        unsafe { (self.drop)(self.ptr) }
    }
}
