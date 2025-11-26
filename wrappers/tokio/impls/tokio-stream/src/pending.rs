use crate::Stream;

use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

/// Stream for the [`pending`](fn@pending) function.
#[derive(Debug)]
#[must_use = "streams do nothing unless polled"]
pub struct Pending<T>(PhantomData<T>);

impl<T> Unpin for Pending<T> {}
unsafe impl<T> Send for Pending<T> {}
unsafe impl<T> Sync for Pending<T> {}

/// Creates a stream that is never ready
///
/// The returned stream is never ready. Attempting to call
/// [`next()`](crate::StreamExt::next) will never complete. Use
/// [`stream::empty()`](super::empty()) to obtain a stream that is is
/// immediately empty but returns no values.
pub const fn pending<T>() -> Pending<T> {
    Pending(PhantomData)
}

impl<T> Stream for Pending<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<T>> {
        Poll::Pending
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, None)
    }
}
