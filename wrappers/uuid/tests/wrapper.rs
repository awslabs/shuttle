//! Tests of the wrapper as a dependent crate sees it. The workspace builds it without the `shuttle`
//! feature, so these check that it then is uuid; `cargo test -p shuttle-uuid --features shuttle`
//! runs the other half. `shuttle-uuid-inner`'s tests run the same consumer code under Shuttle.

extern crate shuttle_uuid as uuid;

mod consumer;

/// Stands in for a crate built on uuid itself.
mod real_crate {
    pub struct Record {
        pub id: uuid_orig::Uuid,
    }

    pub fn echo(uuid: uuid_orig::Uuid) -> uuid_orig::Uuid {
        uuid
    }

    pub fn echo_non_nil(uuid: uuid_orig::NonNilUuid) -> uuid_orig::NonNilUuid {
        uuid
    }
}

#[cfg(not(feature = "shuttle"))]
mod without_shuttle {
    use std::any::TypeId;

    #[test]
    fn types_are_uuids_own() {
        assert_eq!(TypeId::of::<uuid::Uuid>(), TypeId::of::<uuid_orig::Uuid>());
        assert_eq!(TypeId::of::<uuid::NonNilUuid>(), TypeId::of::<uuid_orig::NonNilUuid>());
        assert_eq!(TypeId::of::<uuid::Builder>(), TypeId::of::<uuid_orig::Builder>());
        assert_eq!(TypeId::of::<uuid::Timestamp>(), TypeId::of::<uuid_orig::Timestamp>());
        assert_eq!(TypeId::of::<uuid::ContextV7>(), TypeId::of::<uuid_orig::ContextV7>());
        assert_eq!(
            TypeId::of::<uuid::fmt::Hyphenated>(),
            TypeId::of::<uuid_orig::fmt::Hyphenated>()
        );
    }

    #[test]
    fn consumer_code_runs_on_uuid() {
        super::consumer::exercise();
    }
}

#[cfg(feature = "shuttle")]
mod with_shuttle {
    use std::any::TypeId;
    use uuid::Uuid;

    #[test]
    fn types_are_shuttles_own() {
        assert_ne!(TypeId::of::<Uuid>(), TypeId::of::<uuid_orig::Uuid>());
    }

    #[test]
    fn consumer_code_runs_under_shuttle() {
        shuttle::check_random(super::consumer::exercise, 20);
    }

    #[test]
    fn consumer_code_runs_under_dfs() {
        shuttle::check_dfs(super::consumer::exercise, None);
    }

    fn uuids_with_seed(seed: u64) -> Vec<Uuid> {
        let uuids = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let uuids_clone = uuids.clone();
        shuttle::check_random_with_seed(
            move || {
                let mut uuids = uuids_clone.lock().unwrap();
                uuids.push(Uuid::new_v4());
                uuids.push(Uuid::now_v7());
            },
            seed,
            10,
        );
        uuids.lock().unwrap().clone()
    }

    #[test]
    fn same_seed_gives_the_same_uuids() {
        assert_eq!(uuids_with_seed(1), uuids_with_seed(1));
        assert_ne!(uuids_with_seed(1), uuids_with_seed(2));
    }

    #[test]
    #[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
    fn new_v4_outside_of_shuttle_panics() {
        Uuid::new_v4();
    }
}
