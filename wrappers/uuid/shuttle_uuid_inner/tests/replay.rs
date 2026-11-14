//! This test is alone in its binary because Shuttle's panic hook keeps the `Config` of the first
//! execution in the process, so `FailurePersistence::File` takes effect only if no other test has run
//! a Shuttle execution in the same process first.

mod common;

use common::uuids_from_threads;
use shuttle::scheduler::RandomScheduler;
use shuttle::{Config, FailurePersistence, Runner, replay_from_file};
use shuttle_uuid_inner::Uuid;
use std::panic::{self, AssertUnwindSafe};

#[test]
fn failing_schedule_replays_with_the_same_uuids() {
    // The UUIDs of the latest execution.
    static UUIDS: std::sync::Mutex<Vec<Uuid>> = std::sync::Mutex::new(Vec::new());

    // Fails when the two threads' UUIDs start with the same hex digit, one execution in 16.
    fn first_digits_differ() {
        let uuids = uuids_from_threads(2, Uuid::new_v4);
        *UUIDS.lock().unwrap() = uuids.clone();
        let first_digits = uuids.iter().map(|uuid| uuid.as_bytes()[0] >> 4).collect::<Vec<_>>();
        assert_ne!(first_digits[0], first_digits[1], "UUIDs start with the same digit");
    }

    let schedules = tempfile::tempdir().unwrap();
    let mut config = Config::new();
    config.failure_persistence = FailurePersistence::File(Some(schedules.path().to_path_buf()));
    let runner = Runner::new(RandomScheduler::new(1000), config);
    panic::catch_unwind(AssertUnwindSafe(|| runner.run(first_digits_differ))).expect_err("test should fail");
    let failing = UUIDS.lock().unwrap().clone();

    UUIDS.lock().unwrap().clear();
    let schedule = schedules.path().join("schedule000.txt");
    panic::catch_unwind(|| replay_from_file(first_digits_differ, &schedule)).expect_err("replay should fail");
    assert_eq!(*UUIDS.lock().unwrap(), failing);
}
