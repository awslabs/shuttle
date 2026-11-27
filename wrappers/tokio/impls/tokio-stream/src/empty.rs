use crate::Stream;

use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

/// Stream for the [`empty`](fn@empty) function.
#[derive(Debug)]
#[must_use = "streams do nothing unless polled"]
pub struct Empty<T>(PhantomData<T>);

impl<T> Unpin for Empty<T> {}

// SAFETY:
// In order for `Empty<T>` to be `Send` for every `T`, we need to ensure:
// * moving an `Empty<T>` to another thread moves no value whose type is not `Send`.
//
// An `Empty<T>` holds only a `PhantomData<T>`, which holds no data. It never makes a `T`, as
// `poll_next` always returns `Poll::Ready(None)`.
unsafe impl<T> Send for Empty<T> {}

// SAFETY:
// In order for `Empty<T>` to be `Sync` for every `T`, we need to ensure:
// * threads that share a `&Empty<T>` get no access to a value whose type is not `Sync`.
//
// An `Empty<T>` holds no data, and its `&self` methods, `size_hint` and the derived `Debug`, read
// none.
unsafe impl<T> Sync for Empty<T> {}

/// Creates a stream that yields nothing.
///
/// The returned stream is immediately ready and returns `None`. Use
/// [`stream::pending()`](super::pending()) to obtain a stream that is never
/// ready.
///
pub const fn empty<T>() -> Empty<T> {
    Empty(PhantomData)
}

impl<T> Stream for Empty<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<T>> {
        Poll::Ready(None)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(0))
    }
}
