//! The `std::sync::atomic` types and `fence`, invisible to Shuttle.
//!
//! Each type is an alias of the `std` type, so it is the `std` type itself, and [`fence`] calls
//! the `std` function.

/// [`std::sync::atomic::AtomicBool`], invisible to Shuttle.
pub type AtomicBool = std::sync::atomic::AtomicBool;

/// [`std::sync::atomic::AtomicU8`], invisible to Shuttle.
pub type AtomicU8 = std::sync::atomic::AtomicU8;

/// [`std::sync::atomic::AtomicU16`], invisible to Shuttle.
pub type AtomicU16 = std::sync::atomic::AtomicU16;

/// [`std::sync::atomic::AtomicU32`], invisible to Shuttle.
pub type AtomicU32 = std::sync::atomic::AtomicU32;

/// [`std::sync::atomic::AtomicU64`], invisible to Shuttle.
pub type AtomicU64 = std::sync::atomic::AtomicU64;

/// [`std::sync::atomic::AtomicUsize`], invisible to Shuttle.
pub type AtomicUsize = std::sync::atomic::AtomicUsize;

/// [`std::sync::atomic::AtomicI8`], invisible to Shuttle.
pub type AtomicI8 = std::sync::atomic::AtomicI8;

/// [`std::sync::atomic::AtomicI16`], invisible to Shuttle.
pub type AtomicI16 = std::sync::atomic::AtomicI16;

/// [`std::sync::atomic::AtomicI32`], invisible to Shuttle.
pub type AtomicI32 = std::sync::atomic::AtomicI32;

/// [`std::sync::atomic::AtomicI64`], invisible to Shuttle.
pub type AtomicI64 = std::sync::atomic::AtomicI64;

/// [`std::sync::atomic::AtomicIsize`], invisible to Shuttle.
pub type AtomicIsize = std::sync::atomic::AtomicIsize;

/// [`std::sync::atomic::AtomicPtr`], invisible to Shuttle.
pub type AtomicPtr<T> = std::sync::atomic::AtomicPtr<T>;

// The lint gate doesn't deny `Ordering`, and Shuttle's atomics use the same enum, so a plain
// re-export passes the gate and keeps imports such as `Ordering::{Acquire, Release}` working.
pub use std::sync::atomic::Ordering;

/// [`std::sync::atomic::fence`], invisible to Shuttle.
///
/// It orders the real hardware accesses of the atomics in this module, which is what code using
/// them needs. Like std's, it panics on [`Ordering::Relaxed`]. rustc's `invalid_atomic_ordering`
/// lint rejects a literal `Relaxed` passed to std's `fence`, but it can't see through this
/// wrapper, so here the mistake shows up only when the line runs. Shuttle's `fence` differs in
/// two ways this one doesn't copy: it is a no-op in Shuttle's execution model, and it warns on
/// any ordering other than [`Ordering::SeqCst`].
///
/// `compiler_fence` needs no backdoor: Shuttle re-exports the `std` one, since it has no
/// run-time behavior to model.
#[inline]
pub fn fence(order: Ordering) {
    std::sync::atomic::fence(order)
}
