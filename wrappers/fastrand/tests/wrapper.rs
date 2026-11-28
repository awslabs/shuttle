//! Tests of the wrapper as a dependent crate sees it. The workspace builds it without the `shuttle` feature, so
//! these check that it then is fastrand; `cargo test -p shuttle-fastrand --features shuttle` runs the other half.
//! `shuttle-fastrand-inner`'s tests run the same consumer code under Shuttle.

extern crate shuttle_fastrand as fastrand;

mod consumer;

#[cfg(not(feature = "shuttle"))]
mod without_shuttle {
    use std::any::TypeId;

    #[test]
    fn rng_is_fastrands_own() {
        assert_eq!(TypeId::of::<fastrand::Rng>(), TypeId::of::<fastrand_orig::Rng>());
    }

    #[test]
    fn consumer_code_runs_on_fastrand() {
        super::consumer::exercise();
    }
}

#[cfg(feature = "shuttle")]
mod with_shuttle {
    use std::any::TypeId;

    #[test]
    fn rng_is_shuttles_own() {
        assert_ne!(TypeId::of::<fastrand::Rng>(), TypeId::of::<fastrand_orig::Rng>());
    }

    #[test]
    fn consumer_code_runs_under_shuttle() {
        shuttle::check_random(super::consumer::exercise, 20);
    }

    #[test]
    fn consumer_code_replays() {
        shuttle::check_uncontrolled_nondeterminism(super::consumer::exercise, 20);
    }

    fn values_with_seed(seed: u64) -> Vec<(u64, char, u64)> {
        let values = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let values_clone = values.clone();
        shuttle::check_random_with_seed(
            move || {
                let mut forked = fastrand::Rng::new();
                values_clone
                    .lock()
                    .unwrap()
                    .push((fastrand::u64(..), fastrand::alphanumeric(), forked.u64(..)));
            },
            seed,
            10,
        );
        values.lock().unwrap().clone()
    }

    #[test]
    fn same_seed_gives_the_same_values() {
        assert_eq!(values_with_seed(1), values_with_seed(1));
        assert_ne!(values_with_seed(1), values_with_seed(2));
    }
}
