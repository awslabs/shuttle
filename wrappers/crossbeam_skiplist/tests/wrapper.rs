//! Tests of the wrapper as a dependent crate sees it, through the name `crossbeam_skiplist`. The
//! workspace builds it without the `shuttle` feature, so these check that the wrapper then is
//! crossbeam-skiplist; `cargo test -p shuttle-crossbeam-skiplist --features shuttle` runs the other
//! half. `shuttle-crossbeam-skiplist-inner`'s tests run the same consumer code under Shuttle.

// The consumer code names this crate `crossbeam_skiplist`, as a dependent crate would.
extern crate shuttle_crossbeam_skiplist as crossbeam_skiplist;

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
            TypeId::of::<crossbeam_skiplist::SkipMap<u8, u8>>(),
            TypeId::of::<crossbeam_skiplist_orig::SkipMap<u8, u8>>()
        );
        assert_eq!(
            TypeId::of::<crossbeam_skiplist::SkipSet<u8>>(),
            TypeId::of::<crossbeam_skiplist_orig::SkipSet<u8>>()
        );
        // Without the feature, the guard-based API is there too.
        assert_eq!(
            TypeId::of::<crossbeam_skiplist::SkipList<u8, u8>>(),
            TypeId::of::<crossbeam_skiplist_orig::base::SkipList<u8, u8>>()
        );
    }

    #[test]
    fn consumer_code_runs_on_crossbeam() {
        super::consumer::exercise();
    }
}

#[cfg(feature = "shuttle")]
mod with_shuttle {
    use crossbeam_skiplist::{SkipMap, SkipSet};
    use std::any::TypeId;

    #[test]
    fn types_are_shuttles_own() {
        assert_ne!(
            TypeId::of::<SkipMap<u8, u8>>(),
            TypeId::of::<crossbeam_skiplist_orig::SkipMap<u8, u8>>()
        );
        assert_ne!(
            TypeId::of::<SkipSet<u8>>(),
            TypeId::of::<crossbeam_skiplist_orig::SkipSet<u8>>()
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
    fn an_operation_outside_of_shuttle_panics() {
        SkipMap::<u8, u8>::new().len();
    }

    #[test]
    fn building_works_outside_of_shuttle() {
        let map: SkipMap<u8, u8> = (0..3).map(|i| (i, i)).collect();
        assert_eq!(format!("{map:?}"), "SkipMap { .. }");
        assert_eq!(map.into_iter().collect::<Vec<_>>(), vec![(0, 0), (1, 1), (2, 2)]);
    }
}
