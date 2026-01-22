pub use std::future::poll_fn;

// SHUTTLE_CHANGES: Changed to use rand from Shuttle
#[doc(hidden)]
pub fn thread_rng_n(n: u32) -> u32 {
    use shuttle::rand::RngCore;
    shuttle::rand::thread_rng().next_u32() % n
}

// SHUTTLE_CHANGES: Changed to always be `Poll::Ready(())` (ie. same as Tokio when coop is not enabled). We don't model cooperative scheduling.
#[doc(hidden)]
#[inline]
pub fn poll_budget_available(_: &mut Context<'_>) -> Poll<()> {
    Poll::Ready(())
}

pub use std::future::{Future, IntoFuture};
pub use std::pin::Pin;
pub use std::task::{Context, Poll};
