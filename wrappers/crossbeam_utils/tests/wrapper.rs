//! Tests of the wrapper as a dependent crate sees it. The workspace builds it without the `shuttle`
//! feature, so these check that the wrapper then is crossbeam-utils, and run the consumer code on
//! real threads; `cargo test -p shuttle-crossbeam-utils --features shuttle` runs the same code
//! under Shuttle. `shuttle-crossbeam-utils-inner`'s tests run it under Shuttle too.

pub use shuttle_crossbeam_utils as crossbeam_utils;

#[cfg(not(feature = "shuttle"))]
pub use std::{sync, thread, time};

#[cfg(feature = "shuttle")]
pub use shuttle::{sync, thread};
#[cfg(feature = "shuttle")]
pub use shuttle_time::time;

mod consumer;

#[cfg(not(feature = "shuttle"))]
mod without_shuttle {
    use std::any::TypeId;

    #[test]
    fn items_are_crossbeams_own() {
        assert_eq!(
            TypeId::of::<crate::crossbeam_utils::Backoff>(),
            TypeId::of::<crossbeam_utils_orig::Backoff>()
        );
        assert_eq!(
            TypeId::of::<crate::crossbeam_utils::atomic::AtomicCell<u8>>(),
            TypeId::of::<crossbeam_utils_orig::atomic::AtomicCell<u8>>()
        );
        assert_eq!(
            TypeId::of::<crate::crossbeam_utils::sync::ShardedLock<u8>>(),
            TypeId::of::<crossbeam_utils_orig::sync::ShardedLock<u8>>()
        );
    }

    #[test]
    fn consumer_code_runs_on_real_threads() {
        super::consumer::exercise();
    }
}

#[cfg(feature = "shuttle")]
mod with_shuttle {
    use std::any::TypeId;

    #[test]
    fn primitives_are_shuttles() {
        assert_ne!(
            TypeId::of::<crate::crossbeam_utils::sync::ShardedLock<u8>>(),
            TypeId::of::<crossbeam_utils_orig::sync::ShardedLock<u8>>()
        );
        assert_ne!(
            TypeId::of::<crate::crossbeam_utils::atomic::AtomicCell<u8>>(),
            TypeId::of::<crossbeam_utils_orig::atomic::AtomicCell<u8>>()
        );
        // `CachePadded` is crossbeam's own in both modes.
        assert_eq!(
            TypeId::of::<crate::crossbeam_utils::CachePadded<u8>>(),
            TypeId::of::<crossbeam_utils_orig::CachePadded<u8>>()
        );
        // The feature switches shuttle-time to Shuttle's clock, whose `Instant` the parker takes.
        assert_ne!(TypeId::of::<crate::time::Instant>(), TypeId::of::<std::time::Instant>());
    }

    #[test]
    fn consumer_code_runs_under_shuttle() {
        shuttle::check_random(super::consumer::exercise, 20);
    }

    #[test]
    fn consumer_code_replays() {
        shuttle::check_uncontrolled_nondeterminism(super::consumer::exercise, 20);
    }
}
