use core::future::Future;
use core::mem::MaybeUninit;
use core::pin::Pin;
use core::task::{Context, Poll};

/// A wrapper type that tells the compiler that the contents might not be valid.
///
/// This is necessary mainly when `T` contains a reference. In that case, the
/// compiler will sometimes assume that the reference is always valid; in some
/// cases it will assume this even after the destructor of `T` runs. For
/// example, when a reference is used as a function argument, then the compiler
/// will assume that the reference is valid until the function returns, even if
/// the reference is destroyed during the function. When the reference is used
/// as part of a self-referential struct, that assumption can be false. Wrapping
/// the reference in this type prevents the compiler from making that
/// assumption.
///
/// # Invariants
///
/// The `MaybeUninit` will always contain a valid value until the destructor runs.
//
// Reference
// See <https://users.rust-lang.org/t/unsafe-code-review-semi-owning-weak-rwlock-t-guard/95706>
//
// TODO: replace this with an official solution once RFC #3336 or similar is available.
// <https://github.com/rust-lang/rfcs/pull/3336>
#[repr(transparent)]
pub(crate) struct MaybeDangling<T>(MaybeUninit<T>);

impl<T> Drop for MaybeDangling<T> {
    fn drop(&mut self) {
        // SAFETY:
        // In order for the call to be safe, we need to ensure:
        // * `self.0` holds an initialized `T`.
        // * nothing uses that `T` after it is dropped.
        //
        // `new` is the only way to make a `MaybeDangling`, and it stores a `T`. No method moves
        // the `T` out or drops it. So `self.0` holds it until this destructor runs, which is the
        // invariant in the type's docs.
        // The `MaybeDangling` is being dropped, so nothing uses it afterwards.
        unsafe { core::ptr::drop_in_place(self.0.as_mut_ptr()) };
    }
}

impl<T> MaybeDangling<T> {
    pub(crate) fn new(inner: T) -> Self {
        Self(MaybeUninit::new(inner))
    }
}

impl<F: Future> Future for MaybeDangling<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY:
        // In order for the call to be safe, we need to ensure:
        // * `self.0` holds an initialized `F`.
        // * the `F` stays where it is until it is dropped, and the closure moves nothing out.
        //
        // `self.0` holds an `F`, for the reason that `drop` gives.
        // The `F` is a field of the pinned `MaybeDangling`, so it does not move while the
        // `MaybeDangling` does not. `drop` drops it in place, and no method moves it out. The
        // closure only borrows it. `MaybeDangling<F>` is `Unpin` only if `F` is, because
        // `MaybeUninit<F>` is.
        let fut = unsafe { self.map_unchecked_mut(|this| this.0.assume_init_mut()) };
        fut.poll(cx)
    }
}

#[test]
fn maybedangling_runs_drop() {
    struct SetOnDrop<'a>(&'a mut bool);

    impl Drop for SetOnDrop<'_> {
        fn drop(&mut self) {
            *self.0 = true;
        }
    }

    let mut success = false;

    drop(MaybeDangling::new(SetOnDrop(&mut success)));
    assert!(success);
}
