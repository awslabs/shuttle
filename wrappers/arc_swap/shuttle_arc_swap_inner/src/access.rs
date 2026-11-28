//! Abstracting over accessing parts of stored value.
//!
//! Sometimes, there's a big globalish data structure (like a configuration for the whole program).
//! Then there are parts of the program that need access to up-to-date version of their *part* of
//! the configuration, but for reasons of code separation and reusability, it is not desirable to
//! pass the whole configuration to each of the parts.
//!
//! This module provides means to grant the parts access to the relevant subsets of such global
//! data structure while masking the fact it is part of the bigger whole from the component.
//!
//! Note that the [`cache`](crate::cache) module has its own [`Access`](crate::cache::Access) trait
//! that serves a similar purpose, but with cached access. The signatures are different, therefore
//! an incompatible trait.
//!
//! # The general idea
//!
//! Each part of the code accepts generic [`Access<T>`][Access] for the `T` of its interest. This
//! provides means to load current version of the structure behind the scenes and get only the
//! relevant part, without knowing what the big structure is.
//!
//! For technical reasons, the [`Access`] trait is not object safe. If type erasure is desired, it
//! is possible use the [`DynAccess`] instead, which is object safe, but slightly slower.
//!
//! For some cases, it is possible to use [`ArcSwapAny::map`](crate::ArcSwapAny::map). If that is
//! not flexible enough, the [`Map`] type can be created directly.
//!
//! Note that the [`Access`] trait is also implemented for [`ArcSwapAny`] itself. Additionally,
//! there's the [`Constant`] helper type, which is useful mostly for testing (it doesn't allow
//! reloading).
//!
//! Under Shuttle every `load` through these is the storage's own
//! [`load`](crate::ArcSwapAny::load): one scheduling point and one step.

#![deny(unsafe_code)]

use crate::ref_cnt::RefCnt;
use crate::strategy::Strategy;
use crate::{ArcSwapAny, Guard};
use std::marker::PhantomData;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;

/// Abstracts over ways code can get access to a value of type `T`.
///
/// This is not object safe; see [`DynAccess`] for an object safe version.
pub trait Access<T> {
    /// A deref-able type that holds the inner `T`.
    type Guard: Deref<Target = T>;

    /// The loading method.
    fn load(&self) -> Self::Guard;
}

impl<T, A: Access<T> + ?Sized, P: Deref<Target = A>> Access<T> for P {
    type Guard = A::Guard;
    fn load(&self) -> Self::Guard {
        self.deref().load()
    }
}

impl<T> Access<T> for dyn DynAccess<T> + '_ {
    type Guard = DynGuard<T>;

    fn load(&self) -> Self::Guard {
        self.load()
    }
}

impl<T> Access<T> for dyn DynAccess<T> + '_ + Send {
    type Guard = DynGuard<T>;

    fn load(&self) -> Self::Guard {
        self.load()
    }
}

impl<T> Access<T> for dyn DynAccess<T> + '_ + Sync + Send {
    type Guard = DynGuard<T>;

    fn load(&self) -> Self::Guard {
        self.load()
    }
}

impl<T: RefCnt, S: Strategy<T>> Access<T> for ArcSwapAny<T, S> {
    type Guard = Guard<T, S>;

    fn load(&self) -> Self::Guard {
        self.load()
    }
}

/// A `Guard` that dereferences to the value the smart pointer points at, rather than to the smart
/// pointer.
#[derive(Debug)]
#[doc(hidden)]
pub struct DirectDeref<T: RefCnt, S: Strategy<T>>(Guard<T, S>);

impl<T, S: Strategy<Arc<T>>> Deref for DirectDeref<Arc<T>, S> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0.deref().deref()
    }
}

impl<T, S: Strategy<Arc<T>>> Access<T> for ArcSwapAny<Arc<T>, S> {
    type Guard = DirectDeref<Arc<T>, S>;
    fn load(&self) -> Self::Guard {
        DirectDeref(self.load())
    }
}

impl<T, S: Strategy<Rc<T>>> Deref for DirectDeref<Rc<T>, S> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0.deref().deref()
    }
}

impl<T, S: Strategy<Rc<T>>> Access<T> for ArcSwapAny<Rc<T>, S> {
    type Guard = DirectDeref<Rc<T>, S>;
    fn load(&self) -> Self::Guard {
        DirectDeref(self.load())
    }
}

/// The guard of a [`DynAccess`], which is boxed so that the trait is object safe.
#[doc(hidden)]
pub struct DynGuard<T: ?Sized>(Box<dyn Deref<Target = T>>);

impl<T: ?Sized> Deref for DynGuard<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// An object safe version of the [`Access`] trait.
///
/// This can be used instead of the [`Access`] trait in case a type erasure is desired. This has
/// the effect of performance hit (due to boxing of the result and dynamic dispatch), but
/// unblocks the following scenarios:
///
/// * Impossibility to name the type in some situations.
/// * Possibility to accept multiple distinct types of the same `T`.
pub trait DynAccess<T> {
    /// The equivalent of [`Access::load`].
    fn load(&self) -> DynGuard<T>;
}

impl<T, A> DynAccess<T> for A
where
    A: Access<T>,
    A::Guard: 'static,
{
    fn load(&self) -> DynGuard<T> {
        DynGuard(Box::new(Access::load(self)))
    }
}

/// A newtype that turns a `Deref` to a [`DynAccess`] into an [`Access`].
pub struct AccessConvert<D>(pub D);

impl<T, D> Access<T> for AccessConvert<D>
where
    D: Deref,
    D::Target: DynAccess<T>,
{
    type Guard = DynGuard<T>;

    fn load(&self) -> Self::Guard {
        self.0.load()
    }
}

/// The guard of a [`Map`], which applies the projection to the guard it holds.
#[doc(hidden)]
#[derive(Copy, Clone, Debug)]
pub struct MapGuard<G, F, T, R> {
    guard: G,
    projection: F,
    _t: PhantomData<fn(&T) -> &R>,
}

impl<G, F, T, R> Deref for MapGuard<G, F, T, R>
where
    G: Deref<Target = T>,
    F: Fn(&T) -> &R,
{
    type Target = R;
    fn deref(&self) -> &R {
        (self.projection)(&self.guard)
    }
}

/// An adaptor to provide access to a part of larger structure.
///
/// This is the *active* part of this module. Use the [module documentation](crate::access) for
/// details.
#[derive(Copy, Clone, Debug)]
pub struct Map<A, T, F> {
    access: A,
    projection: F,
    _t: PhantomData<fn() -> T>,
}

impl<A, T, F> Map<A, T, F> {
    /// Creates a new instance.
    ///
    /// # Parameters
    ///
    /// * `access`: Access to the bigger structure. This is usually something like `Arc<ArcSwap<T>>`
    ///   or `&ArcSwap<T>`. It is also possible to use [`Constant`].
    /// * `projection`: A function (or closure) responsible to providing a reference into the
    ///   bigger bigger structure, selecting just subset of it. In general, it is expected to be
    ///   *cheap* (like only taking reference).
    pub fn new<R>(access: A, projection: F) -> Self
    where
        F: Fn(&T) -> &R + Clone,
    {
        Map {
            access,
            projection,
            _t: PhantomData,
        }
    }
}

impl<A, F, T, R> Access<R> for Map<A, T, F>
where
    A: Access<T>,
    F: Fn(&T) -> &R + Clone,
{
    type Guard = MapGuard<A::Guard, F, T, R>;
    fn load(&self) -> Self::Guard {
        let guard = self.access.load();
        MapGuard {
            guard,
            projection: self.projection.clone(),
            _t: PhantomData,
        }
    }
}

/// The guard of a [`Constant`].
#[doc(hidden)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ConstantDeref<T>(T);

impl<T> Deref for ConstantDeref<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// Access to a value inside `ArcSwap` by a constant.
///
/// This implements the [`Access`] trait by cloning a value. It is useful in cases when a part of
/// code needs access to the value, but the value is not stored in an `ArcSwap` (for example in
/// tests).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Constant<T>(pub T);

impl<T: Clone> Access<T> for Constant<T> {
    type Guard = ConstantDeref<T>;
    fn load(&self) -> Self::Guard {
        ConstantDeref(self.0.clone())
    }
}
