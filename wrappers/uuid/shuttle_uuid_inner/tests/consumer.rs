//! The code in `shuttle-uuid`'s `tests/consumer`, written against uuid, compiled unchanged against
//! this crate, which is what `shuttle-uuid` is under its `shuttle` feature, and run under Shuttle.

extern crate shuttle_uuid_inner as uuid;

#[path = "../../tests/consumer/mod.rs"]
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

#[test]
fn consumer_code_runs_under_shuttle() {
    shuttle::check_random(consumer::exercise, 20);
}

#[test]
fn consumer_code_replays() {
    shuttle::check_uncontrolled_nondeterminism(consumer::exercise, 20);
}

#[test]
fn consumer_code_runs_under_dfs() {
    shuttle::check_dfs(consumer::exercise, None);
}
