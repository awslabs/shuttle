//! Tests of the wrapper as a dependent crate sees it. The workspace builds it without the `shuttle`
//! feature, so these check that `shuttle_time::time` then is `std::time`;
//! `cargo test -p shuttle-time --features shuttle` runs the other half. `shuttle-time-inner`'s
//! tests run the same consumer code under Shuttle.

pub use shuttle_time::time;

mod consumer;

/// Stands in for a crate built on `std::time` itself.
mod real_crate {
    pub struct Record {
        pub at: std::time::SystemTime,
    }

    pub fn echo(at: std::time::SystemTime) -> std::time::SystemTime {
        at
    }
}

#[cfg(not(feature = "shuttle"))]
mod without_shuttle {
    use std::any::TypeId;

    #[test]
    fn types_are_stds_own() {
        assert_eq!(TypeId::of::<crate::time::Instant>(), TypeId::of::<std::time::Instant>());
        assert_eq!(
            TypeId::of::<crate::time::SystemTime>(),
            TypeId::of::<std::time::SystemTime>()
        );
        assert_eq!(
            TypeId::of::<crate::time::Duration>(),
            TypeId::of::<std::time::Duration>()
        );
    }

    #[test]
    fn consumer_code_runs_on_std() {
        super::consumer::exercise();
    }
}

#[cfg(feature = "shuttle")]
mod with_shuttle {
    use crate::time::{Instant, SystemTime};
    use std::any::TypeId;

    #[test]
    fn types_are_shuttles_own() {
        assert_ne!(TypeId::of::<Instant>(), TypeId::of::<std::time::Instant>());
        assert_ne!(TypeId::of::<SystemTime>(), TypeId::of::<std::time::SystemTime>());
    }

    #[test]
    fn types_are_stds_where_it_has_no_clock() {
        assert_eq!(
            TypeId::of::<crate::time::Duration>(),
            TypeId::of::<std::time::Duration>()
        );
        assert_eq!(
            TypeId::of::<crate::time::SystemTimeError>(),
            TypeId::of::<std::time::SystemTimeError>()
        );
    }

    #[test]
    fn consumer_code_runs_under_shuttle() {
        shuttle::check_random(super::consumer::exercise, 20);
    }

    #[test]
    fn consumer_code_runs_under_dfs() {
        shuttle::check_dfs(super::consumer::exercise, None);
    }

    #[test]
    #[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
    fn instant_now_outside_of_shuttle_panics() {
        Instant::now();
    }

    #[test]
    #[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
    fn system_time_now_outside_of_shuttle_panics() {
        SystemTime::now();
    }
}
