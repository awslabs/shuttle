//! This test is alone in its binary because Shuttle's panic hook keeps the `Config` of the first
//! execution in the process, so `FailurePersistence::File` takes effect only if no other test has
//! run a Shuttle execution in the same process first.

use shuttle::scheduler::RandomScheduler;
use shuttle::{Config, FailurePersistence, Runner, replay_from_file, thread};
use shuttle_crossbeam_epoch_inner::{self as epoch, Atomic, Owned};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;
use std::sync::atomic::Ordering::SeqCst;

#[test]
fn a_failing_schedule_replays_with_the_same_load() {
    static READ: Mutex<Option<bool>> = Mutex::new(None);

    // Fails when the load catches the moment before the store, in some of the executions.
    fn the_load_sees_the_store() {
        let a = std::sync::Arc::new(Atomic::<u64>::null());
        let writer = {
            let a = std::sync::Arc::clone(&a);
            thread::spawn(move || a.store(Owned::new(1), SeqCst))
        };
        let guard = &epoch::pin();
        let seen = a.load(SeqCst, guard).is_null();
        *READ.lock().unwrap() = Some(seen);
        writer.join().unwrap();
        // Tidy: nothing else reaches the object.
        drop(unsafe { std::sync::Arc::try_unwrap(a).ok().unwrap().try_into_owned() });
        assert!(!seen, "the load came in first");
    }

    let schedules = tempfile::tempdir().unwrap();
    let mut config = Config::new();
    config.failure_persistence = FailurePersistence::File(Some(schedules.path().to_path_buf()));
    let runner = Runner::new(RandomScheduler::new(1000), config);
    panic::catch_unwind(AssertUnwindSafe(|| runner.run(the_load_sees_the_store))).expect_err("test should fail");
    assert_eq!(*READ.lock().unwrap(), Some(true));

    *READ.lock().unwrap() = None;
    let schedule = schedules.path().join("schedule000.txt");
    panic::catch_unwind(|| replay_from_file(the_load_sees_the_store, &schedule)).expect_err("replay should fail");
    assert_eq!(*READ.lock().unwrap(), Some(true));
}
