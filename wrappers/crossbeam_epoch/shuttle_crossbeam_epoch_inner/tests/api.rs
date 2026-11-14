//! API parity: every public item of crossbeam-epoch 0.9.21 (read from its source), with its exact
//! signature on `u8` and on `[MaybeUninit<u8>]`, written once and checked against crossbeam-epoch
//! and against this crate. A difference in a signature, a missing method or a missing impl fails to
//! compile for one of the two. `Pointable` is crossbeam-epoch's own trait in both, which the probe
//! also uses.

#![allow(deprecated, dead_code, clippy::type_complexity)]

macro_rules! probe {
    ($name:ident, $krate:ident) => {
        mod $name {
            use std::borrow::{Borrow, BorrowMut};
            use std::fmt::Debug;
            use std::mem::MaybeUninit;
            use std::ops::{Deref, DerefMut};
            use std::sync::atomic::Ordering;
            use $krate::{
                Atomic, Collector, CompareAndSetError, CompareAndSetOrdering, CompareExchangeError, Guard, LocalHandle,
                Owned, Pointable, Pointer, Shared,
            };

            // `Pointable` is crossbeam's own trait. The probe uses `u8` and a dynamically sized array.
            type S = u8;
            type U = [MaybeUninit<u8>];

            fn send_sync<T: Send + Sync>() {}
            fn debug<T: Debug>() {}

            fn impls() {
                send_sync::<Atomic<S>>();
                send_sync::<Owned<S>>();
                send_sync::<Collector>();
                debug::<Atomic<S>>();
                debug::<Owned<S>>();
                debug::<Shared<'static, S>>();
                debug::<Collector>();
                debug::<LocalHandle>();
                debug::<Guard>();
                let _: for<'a, 'b> fn(&'a Atomic<S>, &mut std::fmt::Formatter<'b>) -> std::fmt::Result =
                    <Atomic<S> as std::fmt::Pointer>::fmt;
                let _: for<'a, 'b> fn(&'a Shared<'static, S>, &mut std::fmt::Formatter<'b>) -> std::fmt::Result =
                    <Shared<'static, S> as std::fmt::Pointer>::fmt;
                let _: fn() -> Atomic<S> = <Atomic<S> as Default>::default;
                let _: fn() -> Shared<'static, S> = <Shared<'static, S> as Default>::default;
                let _: fn(&Atomic<S>) -> Atomic<S> = <Atomic<S> as Clone>::clone;
                let _: fn(Owned<S>) -> Atomic<S> = <Atomic<S> as From<Owned<S>>>::from;
                let _: fn(Box<S>) -> Atomic<S> = <Atomic<S> as From<Box<S>>>::from;
                let _: fn(S) -> Atomic<S> = <Atomic<S> as From<S>>::from;
                let _: fn(Shared<'static, S>) -> Atomic<S> = <Atomic<S> as From<Shared<'static, S>>>::from;
                let _: fn(*const S) -> Atomic<S> = <Atomic<S> as From<*const S>>::from;
                let _: fn(S) -> Owned<S> = <Owned<S> as From<S>>::from;
                let _: fn(Box<S>) -> Owned<S> = <Owned<S> as From<Box<S>>>::from;
                let _: fn(&Shared<'static, S>) -> Shared<'static, S> = <Shared<'static, S> as Clone>::clone;
                let _: fn(*const S) -> Shared<'static, S> = <Shared<'static, S> as From<*const S>>::from;
                let _: fn(&Collector) -> Collector = <Collector as Clone>::clone;
                let _: fn(&Collector, &Collector) -> bool = <Collector as PartialEq>::eq;
            }

            fn pointable_api() {
                let _: usize = <S as Pointable>::ALIGN;
                let _: usize = <U as Pointable>::ALIGN;
                // Every `Pointable` is also crossbeam-epoch's `Pointable`.
                fn crossbeam<T: ?Sized + crossbeam_epoch_orig::Pointable>() {}
                crossbeam::<S>();
                crossbeam::<U>();
            }

            fn pointer_api() {
                let _: fn(Owned<S>) -> usize = <Owned<S> as Pointer<S>>::into_usize;
                let _: fn(Shared<'static, S>) -> usize = <Shared<'static, S> as Pointer<S>>::into_usize;
                let _: fn(Owned<U>) -> usize = <Owned<U> as Pointer<U>>::into_usize;
            }

            fn atomic_api() {
                let _: fn(S) -> Atomic<S> = Atomic::<S>::new;
                let _: fn(S) -> Atomic<S> = Atomic::<S>::init;
                let _: fn(usize) -> Atomic<U> = Atomic::<U>::init;
                let _: fn() -> Atomic<S> = Atomic::<S>::null;
                let _: for<'g> fn(&Atomic<S>, Ordering, &'g Guard) -> Shared<'g, S> = Atomic::<S>::load;
                let _: for<'g> fn(&Atomic<S>, &'g Guard) -> Shared<'g, S> = Atomic::<S>::load_consume;
                let _: fn(&Atomic<S>, Owned<S>, Ordering) = Atomic::<S>::store::<Owned<S>>;
                let _: for<'g> fn(&Atomic<S>, Owned<S>, Ordering, &'g Guard) -> Shared<'g, S> =
                    Atomic::<S>::swap::<Owned<S>>;
                let _: for<'g> fn(
                    &Atomic<S>,
                    Shared<'_, S>,
                    Owned<S>,
                    Ordering,
                    Ordering,
                    &'g Guard,
                ) -> Result<Shared<'g, S>, CompareExchangeError<'g, S, Owned<S>>> =
                    Atomic::<S>::compare_exchange::<Owned<S>>;
                let _: for<'g> fn(
                    &Atomic<S>,
                    Shared<'_, S>,
                    Owned<S>,
                    Ordering,
                    Ordering,
                    &'g Guard,
                ) -> Result<Shared<'g, S>, CompareExchangeError<'g, S, Owned<S>>> =
                    Atomic::<S>::compare_exchange_weak::<Owned<S>>;
                let _: for<'g> fn(
                    &Atomic<S>,
                    Shared<'_, S>,
                    Owned<S>,
                    (Ordering, Ordering),
                    &'g Guard,
                ) -> Result<Shared<'g, S>, CompareAndSetError<'g, S, Owned<S>>> =
                    Atomic::<S>::compare_and_set::<(Ordering, Ordering), Owned<S>>;
                let _: for<'g> fn(
                    &Atomic<S>,
                    Shared<'_, S>,
                    Owned<S>,
                    Ordering,
                    &'g Guard,
                ) -> Result<Shared<'g, S>, CompareAndSetError<'g, S, Owned<S>>> =
                    Atomic::<S>::compare_and_set_weak::<Ordering, Owned<S>>;
                let _: for<'g> fn(&Atomic<S>, usize, Ordering, &'g Guard) -> Shared<'g, S> = Atomic::<S>::fetch_and;
                let _: for<'g> fn(&Atomic<S>, usize, Ordering, &'g Guard) -> Shared<'g, S> = Atomic::<S>::fetch_or;
                let _: for<'g> fn(&Atomic<S>, usize, Ordering, &'g Guard) -> Shared<'g, S> = Atomic::<S>::fetch_xor;
                let _: unsafe fn(Atomic<S>) -> Owned<S> = Atomic::<S>::into_owned;
                let _: unsafe fn(Atomic<S>) -> Option<Owned<S>> = Atomic::<S>::try_into_owned;
            }

            fn owned_api() {
                let _: unsafe fn(*mut S) -> Owned<S> = Owned::<S>::from_raw;
                let _: fn(Owned<S>) -> Box<S> = Owned::<S>::into_box;
                let _: fn(S) -> Owned<S> = Owned::<S>::new;
                let _: fn(S) -> Owned<S> = Owned::<S>::init;
                let _: fn(usize) -> Owned<U> = Owned::<U>::init;
                let _: fn(&Owned<S>) -> usize = Owned::<S>::tag;
                let _: fn(Owned<S>, usize) -> Owned<S> = Owned::<S>::with_tag;
                let _: for<'g> fn(Owned<S>, &'g Guard) -> Shared<'g, S> = Owned::<S>::into_shared;
                let _: for<'a> fn(&'a Owned<S>) -> &'a S = <Owned<S> as Deref>::deref;
                let _: for<'a> fn(&'a mut Owned<S>) -> &'a mut S = <Owned<S> as DerefMut>::deref_mut;
                let _: for<'a> fn(&'a Owned<S>) -> &'a S = <Owned<S> as Borrow<S>>::borrow;
                let _: for<'a> fn(&'a mut Owned<S>) -> &'a mut S = <Owned<S> as BorrowMut<S>>::borrow_mut;
                let _: for<'a> fn(&'a Owned<S>) -> &'a S = <Owned<S> as AsRef<S>>::as_ref;
                let _: for<'a> fn(&'a mut Owned<S>) -> &'a mut S = <Owned<S> as AsMut<S>>::as_mut;
            }

            fn shared_api() {
                let _: fn(&Shared<'static, S>) -> *const S = Shared::<S>::as_raw;
                let _: fn() -> Shared<'static, S> = Shared::<S>::null;
                let _: fn(&Shared<'static, S>) -> bool = Shared::<S>::is_null;
                let _: unsafe fn(&Shared<'static, S>) -> &'static S = Shared::<S>::deref;
                let _: unsafe fn(&mut Shared<'static, S>) -> &'static mut S = Shared::<S>::deref_mut;
                let _: unsafe fn(&Shared<'static, S>) -> Option<&'static S> = Shared::<S>::as_ref;
                let _: unsafe fn(Shared<'static, S>) -> Owned<S> = Shared::<S>::into_owned;
                let _: unsafe fn(Shared<'static, S>) -> Option<Owned<S>> = Shared::<S>::try_into_owned;
                let _: fn(&Shared<'static, S>) -> usize = Shared::<S>::tag;
                let _: fn(&Shared<'static, S>, usize) -> Shared<'static, S> = Shared::<S>::with_tag;
            }

            fn guard_api() {
                let _: fn(&Guard, fn() -> ()) = Guard::defer::<fn() -> (), ()>;
                let _: unsafe fn(&Guard, fn() -> ()) = Guard::defer_unchecked::<fn() -> (), ()>;
                let _: unsafe fn(&Guard, Shared<'_, S>) = Guard::defer_destroy::<S>;
                let _: fn(&Guard) = Guard::flush;
                let _: fn(&mut Guard) = Guard::repin;
                let _: fn(&mut Guard, fn() -> ()) -> () = Guard::repin_after::<fn() -> (), ()>;
                let _: fn(&Guard) -> Option<&Collector> = Guard::collector;
                let _: unsafe fn() -> &'static Guard = $krate::unprotected;
            }

            fn collector_api() {
                let _: fn() -> Collector = Collector::new;
                let _: fn(&Collector) -> LocalHandle = Collector::register;
                let _: fn(&LocalHandle) -> Guard = LocalHandle::pin;
                let _: fn(&LocalHandle) -> bool = LocalHandle::is_pinned;
                let _: fn(&LocalHandle) -> &Collector = LocalHandle::collector;
                let _: fn() -> Guard = $krate::pin;
                let _: fn() -> bool = $krate::is_pinned;
                let _: fn() -> &'static Collector = $krate::default_collector;
            }

            fn ordering_api() {
                fn with<O: CompareAndSetOrdering>() {
                    let _: fn(&O) -> Ordering = CompareAndSetOrdering::success;
                    let _: fn(&O) -> Ordering = CompareAndSetOrdering::failure;
                }
                with::<Ordering>();
                with::<(Ordering, Ordering)>();
            }

            fn used() {
                impls();
                pointable_api();
                pointer_api();
                atomic_api();
                owned_api();
                shared_api();
                guard_api();
                collector_api();
                ordering_api();
            }
        }
    };
}

probe!(crossbeam, crossbeam_epoch_orig);
probe!(model, shuttle_crossbeam_epoch_inner);

#[test]
fn pointable_is_crossbeams_own() {
    fn check<T: ?Sized + shuttle_crossbeam_epoch_inner::Pointable + crossbeam_epoch_orig::Pointable>() {}
    check::<u8>();
    check::<[std::mem::MaybeUninit<u8>]>();
}

#[test]
fn the_probe_compiles_for_both() {}
