//! Tests of the wrapper as a dependent crate sees it, through the name `crossbeam_epoch`. The
//! workspace builds it without the `shuttle` feature, so these check that the wrapper then is
//! crossbeam-epoch; `cargo test -p shuttle-crossbeam-epoch --features shuttle` runs the other half.
//! `shuttle-crossbeam-epoch-inner`'s tests run the same consumer code under Shuttle.

// The consumer code names this crate `crossbeam_epoch`, as a dependent crate would.
extern crate shuttle_crossbeam_epoch as crossbeam_epoch;

mod consumer;

#[cfg(not(feature = "shuttle"))]
mod thread {
    pub use std::thread::*;
}

#[cfg(feature = "shuttle")]
mod thread {
    pub use shuttle::thread::*;
}

#[cfg(not(feature = "shuttle"))]
mod without_shuttle {
    use std::any::TypeId;

    #[test]
    fn types_are_crossbeams_own() {
        assert_eq!(
            TypeId::of::<crossbeam_epoch::Atomic<u8>>(),
            TypeId::of::<crossbeam_epoch_orig::Atomic<u8>>()
        );
        assert_eq!(
            TypeId::of::<crossbeam_epoch::Owned<u8>>(),
            TypeId::of::<crossbeam_epoch_orig::Owned<u8>>()
        );
        assert_eq!(
            TypeId::of::<crossbeam_epoch::Collector>(),
            TypeId::of::<crossbeam_epoch_orig::Collector>()
        );
    }

    #[test]
    fn consumer_code_runs_on_crossbeam() {
        super::consumer::exercise();
    }
}

#[cfg(feature = "shuttle")]
mod with_shuttle {
    use crossbeam_epoch::{Atomic, Collector, Owned, Shared};
    use std::any::TypeId;

    #[test]
    fn types_are_shuttles_own() {
        assert_ne!(
            TypeId::of::<Atomic<u8>>(),
            TypeId::of::<crossbeam_epoch_orig::Atomic<u8>>()
        );
        assert_ne!(
            TypeId::of::<Owned<u8>>(),
            TypeId::of::<crossbeam_epoch_orig::Owned<u8>>()
        );
        assert_ne!(
            TypeId::of::<Collector>(),
            TypeId::of::<crossbeam_epoch_orig::Collector>()
        );
    }

    #[test]
    fn the_traits_are_crossbeams_own() {
        // `Pointable`'s impls are crossbeam's, and a `Pointer` is accepted by both crates' `Atomic`s.
        fn pointable<T: ?Sized + crossbeam_epoch_orig::Pointable>() {}
        pointable::<u8>();
        pointable::<[std::mem::MaybeUninit<u8>]>();
        fn takes<P: crossbeam_epoch::Pointer<u8>>(_: P) {}
        takes(Owned::new(1u8));
        takes(Shared::<u8>::null());
        fn also_crossbeams<P: crossbeam_epoch_orig::Pointer<u8>>(_: P) {}
        also_crossbeams(Owned::new(1u8));
    }

    #[test]
    fn consumer_code_runs_under_shuttle() {
        shuttle::check_random(super::consumer::exercise, 20);
    }

    #[test]
    fn consumer_code_replays() {
        shuttle::check_uncontrolled_nondeterminism(super::consumer::exercise, 20);
    }

    #[test]
    #[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
    fn a_load_outside_of_shuttle_panics() {
        // SAFETY: the unprotected guard protects nothing, and the load is what panics.
        unsafe {
            Atomic::new(1u8).load(std::sync::atomic::Ordering::SeqCst, crossbeam_epoch::unprotected());
        }
    }

    #[test]
    fn owned_works_outside_of_shuttle() {
        let owned = Owned::new(1u8);
        assert_eq!(*owned, 1);
        let atomic: Atomic<u8> = Atomic::from(owned);
        // SAFETY: nothing else reaches the object.
        assert_eq!(*unsafe { atomic.into_owned() }, 1);
    }
}
