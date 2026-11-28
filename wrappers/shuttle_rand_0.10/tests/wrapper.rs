//! Tests of the wrapper as a dependent crate sees it. The workspace builds it without the `shuttle` feature, so
//! these check that it then is rand; `cargo test -p shuttle-rand_0_10 --features shuttle` runs the other half.
//! `shuttle-rand_0_10-inner`'s tests run the same consumer code under Shuttle.

extern crate shuttle_rand_0_10 as rand;

mod consumer;

#[cfg(not(feature = "shuttle"))]
mod without_shuttle {
    use std::any::TypeId;

    #[test]
    fn types_are_rands_own() {
        assert_eq!(
            TypeId::of::<rand::rngs::ThreadRng>(),
            TypeId::of::<rand_orig::rngs::ThreadRng>()
        );
        assert_eq!(
            TypeId::of::<rand::rngs::SysRng>(),
            TypeId::of::<rand_orig::rngs::SysRng>()
        );
        assert_eq!(
            TypeId::of::<rand::rngs::StdRng>(),
            TypeId::of::<rand_orig::rngs::StdRng>()
        );
    }

    #[test]
    fn consumer_code_runs_on_rand() {
        super::consumer::exercise();
    }
}

#[cfg(feature = "shuttle")]
mod with_shuttle {
    use std::any::TypeId;

    #[test]
    fn entropy_types_are_shuttles_own() {
        assert_ne!(
            TypeId::of::<rand::rngs::ThreadRng>(),
            TypeId::of::<rand_orig::rngs::ThreadRng>()
        );
        assert_ne!(
            TypeId::of::<rand::rngs::SysRng>(),
            TypeId::of::<rand_orig::rngs::SysRng>()
        );
        // Seeded generators stay rand's.
        assert_eq!(
            TypeId::of::<rand::rngs::StdRng>(),
            TypeId::of::<rand_orig::rngs::StdRng>()
        );
        assert_eq!(
            TypeId::of::<rand::rngs::ChaCha20Rng>(),
            TypeId::of::<rand_orig::rngs::ChaCha20Rng>()
        );
    }

    #[test]
    fn consumer_code_runs_under_shuttle() {
        shuttle::check_random(super::consumer::exercise, 20);
    }

    #[test]
    fn consumer_code_replays() {
        shuttle::check_uncontrolled_nondeterminism(super::consumer::exercise, 20);
    }

    fn values_with_seed(seed: u64) -> Vec<(u64, u32, [u8; 4])> {
        use rand::RngExt;
        let values = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let values_clone = values.clone();
        shuttle::check_random_with_seed(
            move || {
                let mut seeded: rand::rngs::StdRng = rand::make_rng();
                values_clone
                    .lock()
                    .unwrap()
                    .push((rand::random(), rand::random_range(0..1000), seeded.random()));
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
