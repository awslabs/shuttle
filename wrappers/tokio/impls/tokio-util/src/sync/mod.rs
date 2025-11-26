//! Synchronization primitives

mod cancellation_token;
pub use cancellation_token::{
    guard::DropGuard, CancellationToken, WaitForCancellationFuture, WaitForCancellationFutureOwned,
};
