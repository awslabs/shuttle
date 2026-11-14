//! Tests of the wrapper as a dependent crate sees it. The workspace builds it without the `shuttle`
//! feature, so these check that the wrapper then is crossbeam-channel, and run the consumer code on
//! real threads; `cargo test -p shuttle-crossbeam-channel --features shuttle` runs the same code
//! under Shuttle. `shuttle-crossbeam-channel-inner`'s tests run it under Shuttle too.

pub use shuttle_crossbeam_channel as crossbeam_channel;

#[cfg(not(feature = "shuttle"))]
pub use std::{thread, time};

#[cfg(feature = "shuttle")]
pub use shuttle::thread;
#[cfg(feature = "shuttle")]
pub use shuttle_time::time;

mod consumer;

#[cfg(not(feature = "shuttle"))]
mod without_shuttle {
    use std::any::TypeId;

    #[test]
    fn items_are_crossbeams_own() {
        assert_eq!(
            TypeId::of::<crate::crossbeam_channel::Sender<u8>>(),
            TypeId::of::<crossbeam_channel_orig::Sender<u8>>()
        );
        assert_eq!(
            TypeId::of::<crate::crossbeam_channel::Receiver<u8>>(),
            TypeId::of::<crossbeam_channel_orig::Receiver<u8>>()
        );
        assert_eq!(
            TypeId::of::<crate::crossbeam_channel::Select<'static>>(),
            TypeId::of::<crossbeam_channel_orig::Select<'static>>()
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
    fn channels_are_shuttles() {
        assert_ne!(
            TypeId::of::<crate::crossbeam_channel::Sender<u8>>(),
            TypeId::of::<crossbeam_channel_orig::Sender<u8>>()
        );
        // The error types are crossbeam's own in both modes.
        assert_eq!(
            TypeId::of::<crate::crossbeam_channel::RecvTimeoutError>(),
            TypeId::of::<crossbeam_channel_orig::RecvTimeoutError>()
        );
        // The feature switches shuttle-time to Shuttle's clock, whose `Instant` the channels take.
        assert_ne!(TypeId::of::<crate::time::Instant>(), TypeId::of::<std::time::Instant>());
    }

    #[test]
    fn consumer_code_runs_under_shuttle() {
        shuttle::check_random(super::consumer::exercise, 50);
    }

    #[test]
    fn consumer_code_replays() {
        shuttle::check_uncontrolled_nondeterminism(super::consumer::exercise, 50);
    }
}
