//! Tests of the wrapper as a dependent crate sees it. The workspace builds it without the `shuttle`
//! feature, so these check that it then is spin; `cargo test -p shuttle-spin --features shuttle`
//! runs the other half. `shuttle-spin-inner`'s tests run the same consumer code under Shuttle.

extern crate shuttle_spin as spin;

mod consumer;

#[cfg(not(feature = "shuttle"))]
mod without_shuttle {
    use std::any::TypeId;

    #[test]
    fn types_are_spins_own() {
        assert_eq!(TypeId::of::<spin::Mutex<u32>>(), TypeId::of::<spin_orig::Mutex<u32>>());
        assert_eq!(
            TypeId::of::<spin::RwLock<u32>>(),
            TypeId::of::<spin_orig::RwLock<u32>>()
        );
        assert_eq!(TypeId::of::<spin::Once<u32>>(), TypeId::of::<spin_orig::Once<u32>>());
        assert_eq!(TypeId::of::<spin::Lazy<u32>>(), TypeId::of::<spin_orig::Lazy<u32>>());
        assert_eq!(TypeId::of::<spin::Barrier>(), TypeId::of::<spin_orig::Barrier>());
        assert_eq!(
            TypeId::of::<spin::mutex::TicketMutex<u32>>(),
            TypeId::of::<spin_orig::mutex::TicketMutex<u32>>()
        );
    }

    #[test]
    fn consumer_code_runs_on_spin() {
        super::consumer::exercise();
    }
}

#[cfg(feature = "shuttle")]
mod with_shuttle {
    use std::any::TypeId;
    use std::sync::{Mutex, MutexGuard, PoisonError};

    /// The consumer code locks `static`s, whose locks every execution shares, so two of these tests
    /// running at the same time, as `cargo test` runs them, would hold one at once from two
    /// executions, which the lock reports as a panic (see the inner crate's docs). The tests that run
    /// the consumer code take turns, as a consumer's own tests would have to.
    fn one_at_a_time() -> MutexGuard<'static, ()> {
        static TURN: Mutex<()> = Mutex::new(());
        TURN.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[test]
    fn types_are_shuttles_own() {
        assert_ne!(TypeId::of::<spin::Mutex<u32>>(), TypeId::of::<spin_orig::Mutex<u32>>());
        assert_ne!(
            TypeId::of::<spin::RwLock<u32>>(),
            TypeId::of::<spin_orig::RwLock<u32>>()
        );
        assert_ne!(TypeId::of::<spin::Once<u32>>(), TypeId::of::<spin_orig::Once<u32>>());
    }

    #[test]
    fn consumer_code_runs_under_shuttle() {
        let _turn = one_at_a_time();
        shuttle::check_random(super::consumer::exercise, 20);
    }

    #[test]
    fn consumer_code_replays() {
        let _turn = one_at_a_time();
        shuttle::check_uncontrolled_nondeterminism(super::consumer::exercise, 20);
    }
}
