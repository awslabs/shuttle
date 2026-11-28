//! Caching handle into the [ArcSwapAny].
//!
//! The [Cache] keeps a copy of the internal [Arc] for faster access.
//!
//! Under Shuttle a [`Cache::load`] takes one scheduling point: it reads the stored pointer, as a
//! [`load`](ArcSwapAny::load) does, and replaces its copy only if the pointer changed. The copy it
//! keeps holds the value alive, as a [`Guard`](crate::Guard) does, so the value a cache points at
//! is never freed under it, however many times other tasks store a new one. That is the delayed
//! reclamation arc-swap's documentation warns about, and it is the same here.
//!
//! [Arc]: std::sync::Arc

#![deny(unsafe_code)]

use crate::ArcSwapAny;
use crate::ref_cnt::RefCnt;
use crate::strategy::Strategy;
use std::ops::Deref;

/// Caching handle for [`ArcSwapAny`].
///
/// Instead of loading the [`Arc`][std::sync::Arc] on every request from the shared storage, this
/// keeps another copy inside itself. Upon request it only cheaply revalidates it is up to date. If
/// it is, access is significantly faster. If it is stale, the [load_full] is done and the cache
/// value is replaced. Under allmost all circumstances the performance is at least as good as of
/// [load], but it must be kept around and through a mutable reference.
///
/// Under contention, the [load_full] may be slow, therefore this is suitable for read-heavy
/// scenarios.
///
/// [load]: ArcSwapAny::load
/// [load_full]: ArcSwapAny::load_full
#[derive(Clone, Debug)]
pub struct Cache<A, T> {
    arc_swap: A,
    cached: T,
}

impl<A, T, S> Cache<A, T>
where
    A: Deref<Target = ArcSwapAny<T, S>>,
    T: RefCnt,
    S: Strategy<T>,
{
    /// Creates a new caching handle.
    ///
    /// The parameter is something dereferencing into an [`ArcSwapAny`] (eg. either to `ArcSwap` or
    /// `ArcSwapOption`). That can be `ArcSwapAny` itself, a reference to it or even
    /// `Arc<ArcSwapAny>`.
    pub fn new(arc_swap: A) -> Self {
        let cached = arc_swap.load_full();
        Self { arc_swap, cached }
    }

    /// Gives access to the (possibly stale) cached value.
    pub fn arc_swap(&self) -> &A::Target {
        &self.arc_swap
    }

    /// Loads the currently held value.
    ///
    /// This first revalidates the cached value, and loads a new one if the storage holds another
    /// one by now. Takes one scheduling point.
    #[inline]
    pub fn load(&mut self) -> &T {
        self.revalidate();
        self.load_no_revalidate()
    }

    #[inline]
    fn load_no_revalidate(&self) -> &T {
        &self.cached
    }

    #[inline]
    fn revalidate(&mut self) {
        // One scheduling point and one step: the load is linearizable, and the comparison that
        // follows is this task's own business.
        let current = self.arc_swap.load_full();
        if !std::ptr::eq(RefCnt::as_ptr(&self.cached), RefCnt::as_ptr(&current)) {
            self.cached = current;
        }
    }

    /// Turns this cache into a cache with a projection inside the cached value.
    ///
    /// You'd use this in case when some part of code needs access to only a part of the data. The
    /// projection is applied on every load, so it should be cheap (usually just taking a
    /// reference).
    pub fn map<F, U>(self, f: F) -> MapCache<A, T, F>
    where
        F: FnMut(&T) -> &U,
    {
        MapCache {
            inner: self,
            projection: f,
        }
    }
}

/// A variant of [`Access`](crate::access::Access) for caches.
///
/// This is the same idea as [`access::Access`](crate::access::Access), but for the caches, where
/// the load needs a mutable reference.
pub trait Access<T> {
    /// Loads the value from cache.
    ///
    /// This revalidates the value in the cache, then returns the reference to the cached value.
    fn load(&mut self) -> &T;
}

impl<A, T, S> Access<T::Target> for Cache<A, T>
where
    A: Deref<Target = ArcSwapAny<T, S>>,
    T: Deref<Target = <T as RefCnt>::Base> + RefCnt,
    S: Strategy<T>,
{
    fn load(&mut self) -> &T::Target {
        self.load().deref()
    }
}

impl<A, T, S> From<A> for Cache<A, T>
where
    A: Deref<Target = ArcSwapAny<T, S>>,
    T: RefCnt,
    S: Strategy<T>,
{
    fn from(arc_swap: A) -> Self {
        Self::new(arc_swap)
    }
}

/// An implementation of a cache with a projection into the cached value.
///
/// This is created by the [`Cache::map`] method.
#[derive(Clone, Debug)]
pub struct MapCache<A, T, F> {
    inner: Cache<A, T>,
    projection: F,
}

impl<A, T, S, F, U> Access<U> for MapCache<A, T, F>
where
    A: Deref<Target = ArcSwapAny<T, S>>,
    T: RefCnt,
    S: Strategy<T>,
    F: FnMut(&T) -> &U,
{
    fn load(&mut self) -> &U {
        (self.projection)(self.inner.load())
    }
}
