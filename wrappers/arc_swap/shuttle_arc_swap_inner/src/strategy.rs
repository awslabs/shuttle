//! Strategies for protecting the stored value.
//!
//! arc-swap lets the way a loaded value is protected from being freed under the reader be chosen
//! with the `S` type parameter of [`ArcSwapAny`](crate::ArcSwapAny). Its default strategy keeps
//! per-thread "debt" slots, so that a `load` need not touch the value's reference count.
//!
//! Under Shuttle there is one strategy, [`ShuttleStrategy`], which [`DefaultStrategy`] and
//! [`IndependentStrategy`] both name: a loaded value is protected by a clone of the smart pointer
//! itself, which keeps the value alive for as long as the [`Guard`](crate::Guard) lives, and no
//! reclamation is deferred. See the crate documentation for what that changes.
//!
//! As in arc-swap, the traits here are sealed: the strategies this crate provides are the only
//! ones.

use crate::ref_cnt::RefCnt;
use std::borrow::Borrow;

pub(crate) mod sealed {
    use super::*;

    /// A loaded value, held for as long as a `Guard` lives.
    pub trait Protected<T>: Borrow<T> {
        fn into_inner(self) -> T;
        fn from_inner(ptr: T) -> Self;
    }

    /// What a strategy does, which under Shuttle is only to say how a loaded value is held.
    pub trait InnerStrategy<T: RefCnt> {
        type Protected: Protected<T>;
    }

    /// A strategy that supports [`compare_and_swap`](crate::ArcSwapAny::compare_and_swap).
    pub trait CaS<T: RefCnt>: InnerStrategy<T> {}
}

use sealed::Protected;

/// A strategy for protecting the stored value.
///
/// This is only a marker for the strategies this crate provides; it cannot be implemented outside
/// of it.
pub trait Strategy<T: RefCnt>: sealed::InnerStrategy<T> {}
impl<T: RefCnt, S: sealed::InnerStrategy<T>> Strategy<T> for S {}

/// A strategy that can also [`compare_and_swap`](crate::ArcSwapAny::compare_and_swap).
pub trait CaS<T: RefCnt>: sealed::CaS<T> {}
impl<T: RefCnt, S: sealed::CaS<T>> CaS<T> for S {}

/// The strategy every [`ArcSwapAny`](crate::ArcSwapAny) uses under Shuttle: a loaded value is a
/// clone of the smart pointer, so it keeps the value alive by its own reference count.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShuttleStrategy;

/// The default strategy. Under Shuttle, [`ShuttleStrategy`].
pub type DefaultStrategy = ShuttleStrategy;

/// arc-swap's strategy for a value that is loaded rarely. Under Shuttle, [`ShuttleStrategy`].
#[doc(hidden)]
pub type IndependentStrategy = DefaultStrategy;

// A loaded value is the smart pointer itself, as in arc-swap's `rwlock-strategy`.
impl<T: RefCnt> Protected<T> for T {
    #[inline]
    fn from_inner(ptr: T) -> Self {
        ptr
    }

    #[inline]
    fn into_inner(self) -> T {
        self
    }
}

impl<T: RefCnt> sealed::InnerStrategy<T> for ShuttleStrategy {
    type Protected = T;
}

impl<T: RefCnt> sealed::CaS<T> for ShuttleStrategy {}

/// arc-swap's `rwlock-strategy`, which protects the value with a `std::sync::RwLock<()>`. Under
/// Shuttle it protects it as [`ShuttleStrategy`] does, and the lock is never taken, so that code
/// naming it keeps compiling and does not block a Shuttle execution on a lock Shuttle cannot see.
#[cfg(feature = "rwlock-strategy")]
#[cfg_attr(docsrs, doc(cfg(feature = "rwlock-strategy")))]
mod rw_lock {
    use super::*;

    impl<T: RefCnt> sealed::InnerStrategy<T> for std::sync::RwLock<()> {
        type Protected = T;
    }

    impl<T: RefCnt> sealed::CaS<T> for std::sync::RwLock<()> {}
}

/// Some strategies for internal testing.
///
/// # Warning
///
/// They come with no guarantees of correctness, stability, performance or anything at all. *DO NOT
/// USE*.
#[cfg(feature = "internal-test-strategies")]
#[cfg_attr(docsrs, doc(cfg(feature = "internal-test-strategies")))]
#[deprecated(note = "Only for internal testing. Do not use")]
pub mod test_strategies {
    /// Config for no fast slots. Under Shuttle there are no slots, so this says nothing.
    #[derive(Clone, Copy, Default)]
    pub struct NoFastSlots;

    /// A strategy that fills the slots with some crap to make sure we test the fallbacks too.
    /// Under Shuttle, the one strategy there is.
    #[deprecated(note = "Only for internal testing. Do not use")]
    pub type FillFastSlots = super::ShuttleStrategy;
}
