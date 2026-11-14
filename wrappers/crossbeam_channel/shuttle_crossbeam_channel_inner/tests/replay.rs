//! This test is alone in its binary because Shuttle's panic hook keeps the `Config` of the first
//! execution in the process, so `FailurePersistence::File` takes effect only if no other test has
//! run a Shuttle execution in the same process first.

use shuttle::scheduler::RandomScheduler;
use shuttle::{Config, FailurePersistence, Runner, replay_from_file, thread};
use shuttle_crossbeam_channel_inner::{bounded, select, unbounded};
use std::panic::{self, AssertUnwindSafe};

#[test]
fn a_failing_schedule_replays_with_the_same_choices() {
    // The choices of the latest execution.
    static CHOICES: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

    // Fails when the selections chose the second arm three times in a row, so one in eight times.
    fn three_selections() {
        let (s1, r1) = unbounded();
        let (s2, r2) = bounded(1);
        let mut choices = Vec::new();
        for _ in 0..3 {
            s1.send(1u8).unwrap();
            let sender = {
                let s2 = s2.clone();
                thread::spawn(move || s2.send(2u8).unwrap())
            };
            sender.join().unwrap();
            let picked = select! {
                recv(r1) -> msg => msg.unwrap(),
                recv(r2) -> msg => msg.unwrap(),
            };
            let _ = r1.try_recv();
            let _ = r2.try_recv();
            choices.push(picked);
        }
        *CHOICES.lock().unwrap() = choices.clone();
        assert_ne!(choices, [2, 2, 2], "chose the second arm every time");
    }

    let schedules = tempfile::tempdir().unwrap();
    let mut config = Config::new();
    config.failure_persistence = FailurePersistence::File(Some(schedules.path().to_path_buf()));
    let runner = Runner::new(RandomScheduler::new(1000), config);
    panic::catch_unwind(AssertUnwindSafe(|| runner.run(three_selections))).expect_err("test should fail");
    let failing = CHOICES.lock().unwrap().clone();
    assert_eq!(failing, [2, 2, 2]);

    CHOICES.lock().unwrap().clear();
    let schedule = schedules.path().join("schedule000.txt");
    panic::catch_unwind(|| replay_from_file(three_selections, &schedule)).expect_err("replay should fail");
    assert_eq!(*CHOICES.lock().unwrap(), failing);
}
