//! This test is alone in its binary because Shuttle's panic hook keeps the `Config` of the first
//! execution in the process, so `FailurePersistence::File` takes effect only if no other test has
//! run a Shuttle execution in the same process first.

use shuttle::scheduler::RandomScheduler;
use shuttle::{Config, FailurePersistence, Runner, replay_from_file, thread};
use shuttle_crossbeam_deque_inner::{Steal, Worker};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;

#[test]
fn a_failing_schedule_replays_with_the_same_retry() {
    // What the stealer got in the latest execution.
    static RESULT: Mutex<Option<String>> = Mutex::new(None);

    // Fails when the steal retries, which happens when the worker's pop gets in between.
    fn the_steal_does_not_retry() {
        let w = Worker::new_fifo();
        w.push(1);
        w.push(2);
        let s = w.stealer();
        let thief = thread::spawn(move || s.steal());
        let _ = w.pop();
        let result = thief.join().unwrap();
        *RESULT.lock().unwrap() = Some(format!("{result:?}"));
        assert_ne!(result, Steal::Retry, "the steal retried");
    }

    let schedules = tempfile::tempdir().unwrap();
    let mut config = Config::new();
    config.failure_persistence = FailurePersistence::File(Some(schedules.path().to_path_buf()));
    let runner = Runner::new(RandomScheduler::new(1000), config);
    panic::catch_unwind(AssertUnwindSafe(|| runner.run(the_steal_does_not_retry))).expect_err("test should fail");
    assert_eq!(RESULT.lock().unwrap().as_deref(), Some("Retry"));

    *RESULT.lock().unwrap() = None;
    let schedule = schedules.path().join("schedule000.txt");
    panic::catch_unwind(|| replay_from_file(the_steal_does_not_retry, &schedule)).expect_err("replay should fail");
    assert_eq!(RESULT.lock().unwrap().as_deref(), Some("Retry"));
}
