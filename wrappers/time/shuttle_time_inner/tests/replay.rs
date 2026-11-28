//! This test is alone in its binary because Shuttle's panic hook keeps the `Config` of the first
//! execution in the process, so `FailurePersistence::File` takes effect only if no other test has
//! run a Shuttle execution in the same process first.

mod common;

use common::readings_from_threads;
use shuttle::scheduler::RandomScheduler;
use shuttle::{Config, FailurePersistence, Runner, replay_from_file};
use shuttle_time_inner::time::Instant;
use std::panic::{self, AssertUnwindSafe};

#[test]
fn a_failing_schedule_replays_with_the_same_readings() {
    // The readings of the latest execution.
    static READINGS: std::sync::Mutex<Vec<Instant>> = std::sync::Mutex::new(Vec::new());

    // Fails when the clock did not advance between the two threads' readings, half of the time.
    fn the_threads_read_different_times() {
        let readings = readings_from_threads(2, 1);
        *READINGS.lock().unwrap() = readings.clone();
        assert_ne!(readings[0], readings[1], "the two threads read the same time");
    }

    let schedules = tempfile::tempdir().unwrap();
    let mut config = Config::new();
    config.failure_persistence = FailurePersistence::File(Some(schedules.path().to_path_buf()));
    let runner = Runner::new(RandomScheduler::new(1000), config);
    panic::catch_unwind(AssertUnwindSafe(|| runner.run(the_threads_read_different_times)))
        .expect_err("test should fail");
    let failing = READINGS.lock().unwrap().clone();

    READINGS.lock().unwrap().clear();
    let schedule = schedules.path().join("schedule000.txt");
    panic::catch_unwind(|| replay_from_file(the_threads_read_different_times, &schedule))
        .expect_err("replay should fail");
    assert_eq!(*READINGS.lock().unwrap(), failing);
}
