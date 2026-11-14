//! Tests of the wrapper as a dependent crate sees it. The workspace builds it without the `shuttle`
//! feature, so these check that it then is once_cell; `cargo test -p shuttle-once_cell --features
//! shuttle` runs the other half. `shuttle-once_cell-inner`'s tests run the same consumer code under
//! Shuttle.

extern crate shuttle_once_cell as once_cell;

mod consumer;

#[cfg(not(feature = "shuttle"))]
mod without_shuttle {
    use std::any::TypeId;

    #[test]
    fn types_are_once_cells_own() {
        use once_cell_orig as real;
        assert_eq!(
            TypeId::of::<once_cell::sync::OnceCell<u8>>(),
            TypeId::of::<real::sync::OnceCell<u8>>()
        );
        assert_eq!(
            TypeId::of::<once_cell::sync::Lazy<u8>>(),
            TypeId::of::<real::sync::Lazy<u8>>()
        );
        assert_eq!(
            TypeId::of::<once_cell::unsync::OnceCell<u8>>(),
            TypeId::of::<real::unsync::OnceCell<u8>>()
        );
        assert_eq!(
            TypeId::of::<once_cell::race::OnceBox<u8>>(),
            TypeId::of::<real::race::OnceBox<u8>>()
        );
        assert_eq!(
            TypeId::of::<once_cell::race::OnceBool>(),
            TypeId::of::<real::race::OnceBool>()
        );
        assert_eq!(
            TypeId::of::<once_cell::race::OnceNonZeroUsize>(),
            TypeId::of::<real::race::OnceNonZeroUsize>()
        );
        assert_eq!(
            TypeId::of::<once_cell::race::OnceRef<'static, u8>>(),
            TypeId::of::<real::race::OnceRef<'static, u8>>()
        );
    }

    #[test]
    fn consumer_code_runs_on_once_cell() {
        super::consumer::exercise();
        let workers = (0..4)
            .map(|i| std::thread::spawn(move || super::consumer::worker(i)))
            .collect::<Vec<_>>();
        let seen = workers.into_iter().map(|w| w.join().unwrap()).collect::<Vec<_>>();
        super::consumer::check(&seen);
    }
}

#[cfg(feature = "shuttle")]
mod with_shuttle {
    use std::any::TypeId;

    fn exercise() {
        super::consumer::exercise();
        let workers = (0..4)
            .map(|i| shuttle::thread::spawn(move || super::consumer::worker(i)))
            .collect::<Vec<_>>();
        let seen = workers.into_iter().map(|w| w.join().unwrap()).collect::<Vec<_>>();
        super::consumer::check(&seen);
    }

    #[test]
    fn types_are_shuttles_own() {
        use once_cell_orig as real;
        assert_ne!(
            TypeId::of::<once_cell::sync::OnceCell<u8>>(),
            TypeId::of::<real::sync::OnceCell<u8>>()
        );
        assert_ne!(
            TypeId::of::<once_cell::sync::Lazy<u8>>(),
            TypeId::of::<real::sync::Lazy<u8>>()
        );
        assert_ne!(
            TypeId::of::<once_cell::race::OnceBox<u8>>(),
            TypeId::of::<real::race::OnceBox<u8>>()
        );
        assert_ne!(
            TypeId::of::<once_cell::race::OnceBool>(),
            TypeId::of::<real::race::OnceBool>()
        );
        // `unsync` is once_cell's own in both modes.
        assert_eq!(
            TypeId::of::<once_cell::unsync::OnceCell<u8>>(),
            TypeId::of::<real::unsync::OnceCell<u8>>()
        );
        assert_eq!(
            TypeId::of::<once_cell::unsync::Lazy<u8>>(),
            TypeId::of::<real::unsync::Lazy<u8>>()
        );
    }

    #[test]
    fn consumer_code_runs_under_shuttle() {
        shuttle::check_random(exercise, 50);
    }

    #[test]
    fn consumer_code_replays() {
        shuttle::check_uncontrolled_nondeterminism(exercise, 50);
    }

    #[test]
    #[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
    fn statics_outside_of_shuttle_panic() {
        super::consumer::worker(0);
    }
}
