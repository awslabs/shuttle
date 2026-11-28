//! Tests of the wrapper as a dependent crate sees it. The workspace builds it without the `shuttle`
//! feature, so these check that it then is arc-swap; `cargo test -p shuttle-arc-swap --features
//! shuttle` runs the other half. `shuttle-arc-swap-inner`'s tests run the same consumer code under
//! Shuttle.

extern crate shuttle_arc_swap as arc_swap;

mod consumer;

#[cfg(not(feature = "shuttle"))]
mod without_shuttle {
    use std::any::TypeId;
    use std::sync::Arc;

    #[test]
    fn types_are_arc_swaps_own() {
        assert_eq!(
            TypeId::of::<arc_swap::ArcSwap<u32>>(),
            TypeId::of::<arc_swap_orig::ArcSwap<u32>>()
        );
        assert_eq!(
            TypeId::of::<arc_swap::ArcSwapOption<u32>>(),
            TypeId::of::<arc_swap_orig::ArcSwapOption<u32>>()
        );
        assert_eq!(
            TypeId::of::<arc_swap::Guard<Arc<u32>>>(),
            TypeId::of::<arc_swap_orig::Guard<Arc<u32>>>()
        );
    }

    #[test]
    fn consumer_code_runs_on_arc_swap() {
        super::consumer::exercise();
    }
}

#[cfg(feature = "shuttle")]
mod with_shuttle {
    use std::any::TypeId;

    #[test]
    fn types_are_shuttles_own() {
        assert_ne!(
            TypeId::of::<arc_swap::ArcSwap<u32>>(),
            TypeId::of::<arc_swap_orig::ArcSwap<u32>>()
        );
        assert_ne!(
            TypeId::of::<arc_swap::ArcSwapOption<u32>>(),
            TypeId::of::<arc_swap_orig::ArcSwapOption<u32>>()
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
}
