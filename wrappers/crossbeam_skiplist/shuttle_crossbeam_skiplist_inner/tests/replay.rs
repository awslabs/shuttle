//! This test is alone in its binary because Shuttle's panic hook keeps the `Config` of the first
//! execution in the process, so `FailurePersistence::File` takes effect only if no other test has
//! run a Shuttle execution in the same process first.

use shuttle::scheduler::RandomScheduler;
use shuttle::{Config, FailurePersistence, Runner, replay_from_file, thread};
use shuttle_crossbeam_skiplist_inner::SkipMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

#[test]
fn a_failing_schedule_replays_with_the_same_read() {
    // What the reader saw in the latest execution.
    static READ: Mutex<Option<Option<&str>>> = Mutex::new(None);

    // Fails when the reader catches the moment between the removal of the old entry and the
    // insertion of the new one.
    fn the_key_is_always_there() {
        let map = Arc::new(SkipMap::new());
        map.insert("key", "old");
        let writer = {
            let map = Arc::clone(&map);
            thread::spawn(move || {
                map.insert("key", "new");
            })
        };
        let read = map.get("key").map(|entry| *entry.value());
        *READ.lock().unwrap() = Some(read);
        writer.join().unwrap();
        assert!(read.is_some(), "the key was missing");
    }

    let schedules = tempfile::tempdir().unwrap();
    let mut config = Config::new();
    config.failure_persistence = FailurePersistence::File(Some(schedules.path().to_path_buf()));
    let runner = Runner::new(RandomScheduler::new(1000), config);
    panic::catch_unwind(AssertUnwindSafe(|| runner.run(the_key_is_always_there))).expect_err("test should fail");
    assert_eq!(*READ.lock().unwrap(), Some(None));

    *READ.lock().unwrap() = None;
    let schedule = schedules.path().join("schedule000.txt");
    panic::catch_unwind(|| replay_from_file(the_key_is_always_there, &schedule)).expect_err("replay should fail");
    assert_eq!(*READ.lock().unwrap(), Some(None));
}
