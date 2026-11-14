//! This test is alone in its binary because Shuttle's panic hook keeps the
//! `Config` of the first execution in the process, so
//! `FailurePersistence::File` takes effect only if no other test has run a
//! Shuttle execution in the same process first.

#![cfg(target_os = "linux")]

use std::panic::{self, AssertUnwindSafe};

use shuttle::scheduler::RandomScheduler;
use shuttle::{Config, FailurePersistence, Runner, replay_from_file};
use shuttle_io_uring_impl::{IoUring, opcode};

#[test]
fn a_failing_schedule_replays_with_the_same_completions() {
    // The completions of the latest execution, in the order they were reaped.
    static COMPLETIONS: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

    // Fails when the kernel completes the three nops out of order, most of
    // the time.
    fn the_nops_complete_in_order() {
        let ring = IoUring::new(8).unwrap();
        for ud in 1..=3 {
            let nop = opcode::Nop::new().build().user_data(ud);
            // SAFETY: a nop has no buffers.
            unsafe { ring.submission_shared().push(&nop) }.unwrap();
        }
        ring.submit().unwrap();
        let mut order = Vec::new();
        while order.len() < 3 {
            ring.submit_and_wait(1).unwrap();
            // SAFETY: the only CQ handle.
            order.extend(unsafe { ring.completion_shared() }.map(|c| c.user_data()));
        }
        *COMPLETIONS.lock().unwrap() = order.clone();
        assert_eq!(order, [1, 2, 3], "the nops completed out of order");
    }

    let schedules = tempfile::tempdir().unwrap();
    let mut config = Config::new();
    config.failure_persistence = FailurePersistence::File(Some(schedules.path().to_path_buf()));
    let runner = Runner::new(RandomScheduler::new(1000), config);
    panic::catch_unwind(AssertUnwindSafe(|| runner.run(the_nops_complete_in_order))).expect_err("test should fail");
    let failing = COMPLETIONS.lock().unwrap().clone();
    assert_ne!(failing, [1, 2, 3]);

    COMPLETIONS.lock().unwrap().clear();
    let schedule = schedules.path().join("schedule000.txt");
    panic::catch_unwind(|| replay_from_file(the_nops_complete_in_order, &schedule)).expect_err("replay should fail");
    assert_eq!(*COMPLETIONS.lock().unwrap(), failing);
}
