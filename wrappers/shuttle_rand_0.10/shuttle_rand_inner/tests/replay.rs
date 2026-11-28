//! This test is alone in its binary because Shuttle's panic hook keeps the `Config` of the first execution in the
//! process, so `FailurePersistence::File` takes effect only if no other test has run a Shuttle execution in the
//! same process first.

use shuttle::scheduler::RandomScheduler;
use shuttle::sync::{Arc, Mutex};
use shuttle::{Config, FailurePersistence, Runner, replay_from_file, thread};
use shuttle_rand_0_10_inner as rand;
use shuttle_rand_0_10_inner::rngs::StdRng;
use std::panic::{self, AssertUnwindSafe};

#[test]
fn failing_schedule_replays_with_the_same_values() {
    // The values of the latest execution.
    static VALUES: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

    // Fails when the two threads' values have the same last hex digit, one execution in 16.
    fn last_digits_differ() {
        let values = Arc::new(Mutex::new(Vec::new()));
        let threads = [
            || rand::random::<u64>(),
            || rand::RngExt::random::<u64>(&mut rand::make_rng::<StdRng>()),
        ]
        .into_iter()
        .map(|draw| {
            let values = values.clone();
            thread::spawn(move || values.lock().unwrap().push(draw()))
        })
        .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
        let values = values.lock().unwrap().clone();
        *VALUES.lock().unwrap() = values.clone();
        assert_ne!(values[0] & 0xf, values[1] & 0xf, "values end in the same digit");
    }

    let schedules = tempfile::tempdir().unwrap();
    let mut config = Config::new();
    config.failure_persistence = FailurePersistence::File(Some(schedules.path().to_path_buf()));
    let runner = Runner::new(RandomScheduler::new(1000), config);
    panic::catch_unwind(AssertUnwindSafe(|| runner.run(last_digits_differ))).expect_err("test should fail");
    let failing = VALUES.lock().unwrap().clone();

    VALUES.lock().unwrap().clear();
    let schedule = schedules.path().join("schedule000.txt");
    panic::catch_unwind(|| replay_from_file(last_digits_differ, &schedule)).expect_err("replay should fail");
    assert_eq!(*VALUES.lock().unwrap(), failing);
}
