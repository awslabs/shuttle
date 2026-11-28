//! This test is alone in its binary because Shuttle's panic hook keeps the `Config` of the first
//! execution in the process, so `FailurePersistence::File` takes effect only if no other test has
//! run a Shuttle execution in the same process first.

use shuttle::scheduler::RandomScheduler;
use shuttle::{Config, FailurePersistence, Runner, replay_from_file, thread};
use shuttle_crossbeam_queue_inner::SegQueue;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

#[test]
fn a_failing_schedule_replays_with_the_same_pops() {
    // The order in which the latest execution popped the values.
    static ORDER: Mutex<Vec<u32>> = Mutex::new(Vec::new());

    // Fails when the second producer's value comes out first, in some of the executions.
    fn the_first_producer_wins() {
        let q = Arc::new(SegQueue::new());
        let producers: Vec<_> = [1, 2]
            .into_iter()
            .map(|value| {
                let q = Arc::clone(&q);
                thread::spawn(move || q.push(value))
            })
            .collect();
        for producer in producers {
            producer.join().unwrap();
        }
        let order: Vec<u32> = std::iter::from_fn(|| q.pop()).collect();
        *ORDER.lock().unwrap() = order.clone();
        assert_eq!(order, vec![1, 2], "the second producer won");
    }

    let schedules = tempfile::tempdir().unwrap();
    let mut config = Config::new();
    config.failure_persistence = FailurePersistence::File(Some(schedules.path().to_path_buf()));
    let runner = Runner::new(RandomScheduler::new(1000), config);
    panic::catch_unwind(AssertUnwindSafe(|| runner.run(the_first_producer_wins))).expect_err("test should fail");
    let failing = ORDER.lock().unwrap().clone();
    assert_eq!(failing, vec![2, 1]);

    ORDER.lock().unwrap().clear();
    let schedule = schedules.path().join("schedule000.txt");
    panic::catch_unwind(|| replay_from_file(the_first_producer_wins, &schedule)).expect_err("replay should fail");
    assert_eq!(*ORDER.lock().unwrap(), failing);
}
