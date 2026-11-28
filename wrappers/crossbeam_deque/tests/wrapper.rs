//! Tests of the wrapper as a dependent crate sees it, through the name `crossbeam_deque`. The
//! workspace builds it without the `shuttle` feature, so these check that the wrapper then is
//! crossbeam-deque; `cargo test -p shuttle-crossbeam-deque --features shuttle` runs the other half.
//! `shuttle-crossbeam-deque-inner`'s tests run the same consumer code under Shuttle.

// The consumer code names this crate `crossbeam_deque`, as a dependent crate would.
extern crate shuttle_crossbeam_deque as crossbeam_deque;

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
            TypeId::of::<crossbeam_deque::Worker<u8>>(),
            TypeId::of::<crossbeam_deque_orig::Worker<u8>>()
        );
        assert_eq!(
            TypeId::of::<crossbeam_deque::Stealer<u8>>(),
            TypeId::of::<crossbeam_deque_orig::Stealer<u8>>()
        );
        assert_eq!(
            TypeId::of::<crossbeam_deque::Injector<u8>>(),
            TypeId::of::<crossbeam_deque_orig::Injector<u8>>()
        );
    }

    #[test]
    fn consumer_code_runs_on_crossbeam() {
        super::consumer::exercise();
    }
}

#[cfg(feature = "shuttle")]
mod with_shuttle {
    use crossbeam_deque::{Injector, Steal, Stealer, Worker};
    use std::any::TypeId;

    #[test]
    fn types_are_shuttles_own() {
        assert_ne!(
            TypeId::of::<Worker<u8>>(),
            TypeId::of::<crossbeam_deque_orig::Worker<u8>>()
        );
        assert_ne!(
            TypeId::of::<Stealer<u8>>(),
            TypeId::of::<crossbeam_deque_orig::Stealer<u8>>()
        );
        assert_ne!(
            TypeId::of::<Injector<u8>>(),
            TypeId::of::<crossbeam_deque_orig::Injector<u8>>()
        );
    }

    #[test]
    fn steal_is_crossbeams_own() {
        assert_eq!(
            TypeId::of::<Steal<u8>>(),
            TypeId::of::<crossbeam_deque_orig::Steal<u8>>()
        );
    }

    #[test]
    fn consumer_code_runs_under_shuttle() {
        shuttle::check_random(super::consumer::exercise, 50);
    }

    #[test]
    fn consumer_code_replays() {
        shuttle::check_uncontrolled_nondeterminism(super::consumer::exercise, 50);
    }

    #[test]
    #[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
    fn push_outside_of_shuttle_panics() {
        Injector::new().push(1);
    }

    #[test]
    fn construction_works_outside_of_shuttle() {
        let w = Worker::<u8>::new_fifo();
        let s = w.stealer();
        assert_eq!(format!("{w:?} {s:?}"), "Worker { .. } Stealer { .. }");
        drop((w, s, Injector::<u8>::new()));
    }
}
