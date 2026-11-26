//! The wrapper as a dependent crate sees it. The workspace builds it without the `shuttle` feature,
//! so these run the consumer code on OS threads against real `parking_lot`;
//! `cargo test -p shuttle-parking_lot --features shuttle` runs the same source against the Shuttle
//! implementation, inside a Shuttle execution. `shuttle-parking_lot-impl`'s `parity` test runs it
//! against real `parking_lot` by the same source path, and compares the API.

pub use shuttle_parking_lot as parking_lot;

#[cfg(feature = "shuttle")]
pub use shuttle::thread;
#[cfg(not(feature = "shuttle"))]
pub use std::thread;

mod consumer;

#[cfg(not(feature = "shuttle"))]
mod without_shuttle {
    use std::any::TypeId;

    /// The wrapper's types are real `parking_lot`'s, which the test can name because the wrapper
    /// depends on it.
    #[test]
    fn the_types_are_parking_lots_own() {
        assert_eq!(
            TypeId::of::<crate::parking_lot::Mutex<u32>>(),
            TypeId::of::<::parking_lot::Mutex<u32>>()
        );
        assert_eq!(
            TypeId::of::<crate::parking_lot::Condvar>(),
            TypeId::of::<::parking_lot::Condvar>()
        );
        assert_eq!(
            TypeId::of::<crate::parking_lot::ReentrantMutex<u32>>(),
            TypeId::of::<::parking_lot::ReentrantMutex<u32>>()
        );
        assert_eq!(
            TypeId::of::<crate::parking_lot::FairMutex<u32>>(),
            TypeId::of::<::parking_lot::FairMutex<u32>>()
        );
        assert_eq!(
            TypeId::of::<crate::parking_lot::Once>(),
            TypeId::of::<::parking_lot::Once>()
        );
    }

    #[test]
    fn the_consumer_code_runs_on_threads() {
        super::consumer::exercise();
        super::consumer::signatures();
    }

    /// Printed so that the two modes' tables can be compared (see `parity`'s).
    #[test]
    fn auto_traits() {
        println!("{}", super::consumer::auto_traits());
    }
}

#[cfg(feature = "shuttle")]
mod with_shuttle {
    use std::any::TypeId;
    use std::sync::{Mutex, MutexGuard, PoisonError};

    /// The consumer code locks `static`s, whose data every execution shares, so two of these tests
    /// running at the same time, as `cargo test` runs them, would hold one at once from two
    /// executions, which the lock reports as a panic (see the impl crate's docs). The tests that run
    /// the consumer code take turns, as a consumer's own tests would have to.
    fn one_at_a_time() -> MutexGuard<'static, ()> {
        static TURN: Mutex<()> = Mutex::new(());
        TURN.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The wrapper's types are the Shuttle implementation's. (Real `parking_lot`'s are not named
    /// here: the wrapper doesn't use that dependency in this mode, and naming it in the test would
    /// hide cargo's warning that says so.)
    #[test]
    fn the_types_are_the_shuttle_implementations() {
        assert_eq!(
            TypeId::of::<crate::parking_lot::Mutex<u32>>(),
            TypeId::of::<shuttle_parking_lot_impl::Mutex<u32>>()
        );
        assert_eq!(
            TypeId::of::<crate::parking_lot::Condvar>(),
            TypeId::of::<shuttle_parking_lot_impl::Condvar>()
        );
        assert_eq!(
            TypeId::of::<crate::parking_lot::ReentrantMutex<u32>>(),
            TypeId::of::<shuttle_parking_lot_impl::ReentrantMutex<u32>>()
        );
        assert_eq!(
            TypeId::of::<crate::parking_lot::FairMutex<u32>>(),
            TypeId::of::<shuttle_parking_lot_impl::FairMutex<u32>>()
        );
        assert_eq!(
            TypeId::of::<crate::parking_lot::Once>(),
            TypeId::of::<shuttle_parking_lot_impl::Once>()
        );
    }

    #[test]
    fn the_consumer_code_runs_under_shuttle() {
        let _turn = one_at_a_time();
        shuttle::check_random(
            || {
                super::consumer::exercise();
                super::consumer::signatures();
            },
            50,
        );
    }

    #[test]
    fn the_consumer_code_replays() {
        let _turn = one_at_a_time();
        shuttle::check_uncontrolled_nondeterminism(super::consumer::exercise, 20);
    }

    #[test]
    fn auto_traits() {
        println!("{}", super::consumer::auto_traits());
    }
}
