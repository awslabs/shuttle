use crate::basic::clocks::me;
use crate::{check_replay_roundtrip, check_replay_roundtrip_file, Config, FailurePersistence};
use shuttle::scheduler::{PctScheduler, RandomScheduler, ReplayScheduler, RoundRobinScheduler, Schedule};
use shuttle::sync::Mutex;
use shuttle::{replay, thread, Runner};
use std::panic;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use test_log::test;

fn concurrent_increment_buggy() {
    let lock = Arc::new(Mutex::new(0usize));

    let threads = (0..2)
        .map(|_| {
            let lock = Arc::clone(&lock);
            thread::spawn(move || {
                let curr = *lock.lock().unwrap();
                *lock.lock().unwrap() = curr + 1;
            })
        })
        .collect::<Vec<_>>();

    for thd in threads {
        thd.join().unwrap();
    }

    // there's a race where both threads read 0 and then set the counter to 1, so this can fail
    assert_eq!(*lock.lock().unwrap(), 2, "counter is wrong");
}

#[test]
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
#[should_panic(expected = "91021000904092940400")]
fn replay_failing() {
    replay(concurrent_increment_buggy, "91021000904092940400")
}

#[test]
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
fn replay_passing() {
    replay(concurrent_increment_buggy, "9102110090205124480000")
}

#[test]
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
fn replay_roundtrip() {
    check_replay_roundtrip(concurrent_increment_buggy, PctScheduler::new(2, 100))
}

#[test]
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
fn replay_roundtrip_file() {
    check_replay_roundtrip_file(concurrent_increment_buggy, PctScheduler::new(2, 100))
}

fn deadlock() {
    let lock1 = Arc::new(Mutex::new(0usize));
    let lock2 = Arc::new(Mutex::new(0usize));
    let lock1_clone = Arc::clone(&lock1);
    let lock2_clone = Arc::clone(&lock2);

    thread::spawn(move || {
        let _l1 = lock1_clone.lock().unwrap();
        let _l2 = lock2_clone.lock().unwrap();
    });

    let _l2 = lock2.lock().unwrap();
    let _l1 = lock1.lock().unwrap();
}

#[test]
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
fn replay_deadlock_roundtrip() {
    check_replay_roundtrip(deadlock, PctScheduler::new(2, 100))
}

#[test]
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
fn replay_deadlock_roundtrip_file() {
    check_replay_roundtrip_file(deadlock, PctScheduler::new(2, 100))
}

fn deadlock_3() {
    let lock1 = Arc::new(Mutex::new(0usize));
    let lock2 = Arc::new(Mutex::new(0usize));
    let lock3 = Arc::new(Mutex::new(0usize));

    let lock1_clone = Arc::clone(&lock1);
    let lock2_clone = Arc::clone(&lock2);
    let lock3_clone = Arc::clone(&lock3);

    thread::spawn(move || {
        let _l1 = lock1_clone.lock().unwrap();
        let _l2 = lock2_clone.lock().unwrap();
    });

    thread::spawn(move || {
        let _l2 = lock2.lock().unwrap();
        let _l3 = lock3_clone.lock().unwrap();
    });

    let _l3 = lock3.lock().unwrap();
    let _l1 = lock1.lock().unwrap();
}

#[test]
#[should_panic(expected = "deadlock")]
fn replay_deadlock3_block() {
    // Reproduce deadlock
    let schedule = Schedule::new_from_task_ids(0, vec![0, 0, 1, 0, 1, 0, 2, 2, 1]);
    let scheduler = ReplayScheduler::new_from_schedule(schedule);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(deadlock_3);
}

#[test]
fn replay_deadlock3_end_early() {
    // Schedule ends without all tasks finishing
    let schedule = Schedule::new_from_task_ids(0, vec![0, 0, 1, 2]);
    let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
    scheduler.set_allow_incomplete();
    let runner = Runner::new(scheduler, Default::default());
    runner.run(deadlock_3);
}

#[test]
fn replay_deadlock3_task_disabled() {
    // Schedule ends when a task is not runnable
    let schedule = Schedule::new_from_task_ids(0, vec![0, 1, 2, 0]);
    let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
    scheduler.set_allow_incomplete();
    let runner = Runner::new(scheduler, Default::default());
    runner.run(deadlock_3);
}

#[ignore = "this test aborts due to an issue with panic handling on exit with the generators library -- can be removed when we switch to corosensei"]
#[test]
fn replay_deadlock3_drop_mutex() {
    // Schedule ends with a task holding a Mutex, whose MutexGuard needs to be correctly cleaned up
    let schedule = Schedule::new_from_task_ids(0, vec![0, 0, 1, 0, 1, 0]);
    let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
    scheduler.set_allow_incomplete();
    let runner = Runner::new(scheduler, Default::default());
    runner.run(deadlock_3);
}

/// A program that always terminates, and counts its completions in a non-Shuttle counter so that a
/// replay which stops early can be told apart from one that ran the program to the end.
fn concurrent_increment(completions: Arc<AtomicUsize>) -> impl Fn() + Send + Sync + 'static {
    move || {
        let lock = Arc::new(Mutex::new(0usize));

        let threads = (0..3)
            .map(|_| {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    *lock.lock().unwrap() += 1;
                })
            })
            .collect::<Vec<_>>();

        for thd in threads {
            thd.join().unwrap();
        }

        assert_eq!(*lock.lock().unwrap(), 3);
        completions.fetch_add(1, Ordering::SeqCst);
    }
}

/// A schedule that covers only the beginning of the execution ends the replay...
#[test]
#[should_panic(expected = "schedule ended early")]
fn replay_schedule_ends_early() {
    let schedule = Schedule::new_from_task_ids(0, vec![0, 0]);
    let scheduler = ReplayScheduler::new_from_schedule(schedule);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(concurrent_increment(Arc::new(AtomicUsize::new(0))));
}

/// ... unless the scheduler is asked to continue after the schedule, in which case the program runs
/// to completion under the continuation scheduler.
#[test]
fn replay_continue_after_schedule() {
    let completions = Arc::new(AtomicUsize::new(0));

    let schedule = Schedule::new_from_task_ids(0, vec![0, 0]);
    let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
    scheduler.set_continue_after_schedule();
    let runner = Runner::new(scheduler, Default::default());
    runner.run(concurrent_increment(Arc::clone(&completions)));

    assert_eq!(completions.load(Ordering::SeqCst), 1);
}

/// A schedule that diverges from the program (here, by naming a task that will never exist) ends the
/// replay...
#[test]
#[should_panic(expected = "scheduled task is not runnable")]
fn replay_schedule_diverges() {
    let schedule = Schedule::new_from_task_ids(0, vec![0, 7]);
    let scheduler = ReplayScheduler::new_from_schedule(schedule);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(concurrent_increment(Arc::new(AtomicUsize::new(0))));
}

/// ... and is also continued from, rather than abandoning the execution.
#[test]
fn replay_continue_after_diverging_from_schedule() {
    let completions = Arc::new(AtomicUsize::new(0));

    let schedule = Schedule::new_from_task_ids(0, vec![0, 7]);
    let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
    scheduler.set_continue_after_schedule();
    let runner = Runner::new(scheduler, Default::default());
    runner.run(concurrent_increment(Arc::clone(&completions)));

    assert_eq!(completions.load(Ordering::SeqCst), 1);
}

/// The continuation scheduler can be any scheduler, not just the default `RandomScheduler`.
#[test]
fn replay_continue_after_schedule_with_scheduler() {
    let completions = Arc::new(AtomicUsize::new(0));

    let schedule = Schedule::new_from_task_ids(0, vec![0, 0]);
    let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
    scheduler.set_continue_after_schedule_with(RoundRobinScheduler::new(1));
    let runner = Runner::new(scheduler, Default::default());
    runner.run(concurrent_increment(Arc::clone(&completions)));

    assert_eq!(completions.load(Ordering::SeqCst), 1);
}

/// A program that always fails, with a message that depends on both the interleaving and the random
/// values the tasks were given.
fn random_sum_then_fail() {
    use shuttle::rand::Rng;

    let sum = Arc::new(Mutex::new(0u64));

    let threads = (0..3)
        .map(|_| {
            let sum = Arc::clone(&sum);
            thread::spawn(move || {
                for _ in 0..3 {
                    let value = shuttle::rand::thread_rng().gen::<u64>();
                    let mut sum = sum.lock().unwrap();
                    *sum = sum.wrapping_mul(31).wrapping_add(value);
                }
            })
        })
        .collect::<Vec<_>>();

    for thd in threads {
        thd.join().unwrap();
    }

    // If this would be a `panic!`, downcasting the `catch_unwind` error to `String` fails.
    assert_eq!(*sum.lock().unwrap(), 0, "so much work, and all for nothing");
}

/// Run `random_sum_then_fail` to failure and return the panic message, persisting the schedule to
/// `dir` if `dir` is given and replaying `schedule` if `schedule` is given.
fn run_to_failure(schedule: Option<std::path::PathBuf>, dir: Option<std::path::PathBuf>) -> String {
    let result = panic::catch_unwind(move || {
        let mut config = Config::new();
        config.failure_persistence = match dir {
            Some(dir) => FailurePersistence::File(Some(dir)),
            None => FailurePersistence::None,
        };
        match schedule {
            Some(path) => {
                let scheduler = ReplayScheduler::new_from_file(path).expect("could not read schedule file");
                Runner::new(scheduler, config).run(random_sum_then_fail)
            }
            None => {
                // A schedule covering only the start of the execution, continued from.
                let mut scheduler = ReplayScheduler::new_from_schedule(Schedule::new_from_task_ids(0, vec![0, 0]));
                scheduler.set_continue_after_schedule();
                Runner::new(scheduler, config).run(random_sum_then_fail)
            }
        }
    })
    .expect_err("test should panic");
    *result.downcast::<String>().expect("panic payload should be a string")
}

/// A failure found after the end of the recorded schedule is still reported as a schedule that
/// replays the whole execution on its own: the runner records the steps actually taken, and the
/// random values the program was given came from the replayed schedule's seed.
#[test]
fn replay_continue_after_schedule_records_a_replayable_schedule() {
    let dir = tempfile::tempdir().unwrap();

    let expected = run_to_failure(None, Some(dir.path().to_path_buf()));
    assert!(expected.contains("so much work"), "unexpected failure: {expected}");

    // The panic hook that persists schedules is installed process-globally by the first `Runner` to
    // run, so in a test harness that shares one process between tests (plain `cargo test`) the
    // config of whichever test got there first decides where, or whether, the schedule is written.
    // Under `cargo nextest`, which CI uses and which gives each test its own process, it is ours.
    let Some(persisted) = std::fs::read_dir(dir.path()).unwrap().next() else {
        return;
    };

    // The message embeds the sum the tasks computed, so it only matches if the replay of the
    // recorded schedule reproduced the same interleaving *and* the same random values.
    assert_eq!(run_to_failure(Some(persisted.unwrap().path()), None), expected);
}

/// Records the interleaving of a program, and the random values it was given, in a non-Shuttle log.
fn record_interleaving(log: Arc<std::sync::Mutex<Vec<(usize, u64)>>>) -> impl Fn() + Send + Sync + 'static {
    move || {
        let threads = (0..3)
            .map(|_| {
                let log = Arc::clone(&log);
                thread::spawn(move || {
                    use shuttle::rand::Rng;
                    for _ in 0..5 {
                        let value = shuttle::rand::thread_rng().gen::<u64>();
                        log.lock().unwrap().push((me(), value));
                        thread::yield_now();
                    }
                })
            })
            .collect::<Vec<_>>();

        for thd in threads {
            thd.join().unwrap();
        }
    }
}

/// Continuing after the schedule stays deterministic: the continuation scheduler is seeded from the
/// replayed schedule, so the same schedule gives the same interleaving and the same random values.
#[test]
fn replay_continue_after_schedule_is_deterministic() {
    let run = || {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let schedule = Schedule::new_from_task_ids(0, vec![0, 0]);
        let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
        scheduler.set_continue_after_schedule();
        let runner = Runner::new(scheduler, Default::default());
        runner.run(record_interleaving(Arc::clone(&log)));
        let log = std::mem::take(&mut *log.lock().unwrap());
        log
    };

    let first = run();
    assert_eq!(first.len(), 15, "the program ran to completion");
    assert_eq!(first, run());
}

fn long_schedule() {
    let mut threads = vec![];
    for _ in 0..100 {
        threads.push(shuttle::thread::spawn(|| {
            for _ in 0..100 {
                shuttle::thread::yield_now();
            }
        }));
    }
    for t in threads {
        t.join().unwrap();
    }
    // If this would be a `panic!`, downcasting the `catch_unwind` error to `String` fails.
    assert_eq!(1, 2, "so much work, and all for nothing");
}

#[test]
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
fn replay_long_schedule() {
    check_replay_roundtrip(long_schedule, RandomScheduler::new(1));
}

#[test]
#[ignore = "replay mechanism is broken because the schedule is not emitted in the panic output. reintroduce once replay mechanism is fixed."]
fn replay_long_schedule_file() {
    check_replay_roundtrip_file(long_schedule, RandomScheduler::new(1));
}

// Check that FailurePersistence::None does not print a schedule
#[test]
fn replay_persist_none() {
    let result = panic::catch_unwind(|| {
        let scheduler = PctScheduler::new(2, 100);
        let mut config = Config::new();
        config.failure_persistence = FailurePersistence::None;
        let runner = Runner::new(scheduler, config);
        runner.run(concurrent_increment_buggy);
    })
    .expect_err("test should panic");
    let output = result.downcast::<String>().unwrap();
    assert!(output.contains("counter is wrong"));
    // All our current failure persistence modes print the word "schedule", so check that's missing
    assert!(!output.contains("schedule"));
}

/// Tests that events not causally related to the failure are never scheduled.
#[test]
fn replay_causality() {
    // The main thread will spawn three threads:
    // - A, which acquires the lock and sets it to one;
    // - B, which acquires the lock and asserts it is zero;
    // - C, which sets an unrelated atomic Boolean.
    // If A runs before B (as in the schedule below), then the assertion
    // fails. If we provide the clock of this failure to the scheduler, we
    // should never see thread C do anything, i.e., the atomic Boolean should
    // never be set, because it is not causally related to the actual panic.

    use std::sync::atomic::{AtomicBool, Ordering};

    let flag = Arc::new(AtomicBool::new(false));
    let flag_clone = Arc::clone(&flag);

    let result = panic::catch_unwind(|| {
        let schedule = Schedule::new_from_task_ids(0, vec![0, 0, 1, 1, 0, 0, 3, 2, 0, 1, 2, 2]);
        let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
        scheduler.set_target_clock(&[2, 2, 1]);
        let mut config = Config::new();
        config.failure_persistence = FailurePersistence::None;
        let runner = Runner::new(scheduler, config);
        runner.run(move || {
            assert_eq!(me(), 0);
            let lock = Arc::new(Mutex::new(0usize));
            let lock_clone = Arc::clone(&lock);
            thread::spawn(move || {
                assert_eq!(me(), 1);
                *lock_clone.lock().unwrap() = 1;
            });
            thread::spawn(move || {
                assert_eq!(me(), 2);
                let guard = lock.lock().unwrap();
                assert!(*guard == 0, "expected panic");
                drop(guard);
            });
            let flag_clone = Arc::clone(&flag_clone);
            thread::spawn(move || {
                assert_eq!(me(), 3);
                // Note that this operation is performed in a separate thread,
                // since the (non-Shuttle) atomic does not increment the clock
                // of the current thread. If the atomic were set instead in the
                // main thread, then the clocks of "setting the atomic" and "B
                // acquiring a lock" would be indistinguishable. However, we
                // need the non-Shuttle atomic to smuggle data out of a this
                // panicking test.
                flag_clone.store(true, Ordering::SeqCst);
            });
        });
    })
    .expect_err("test should panic");
    let output = result.downcast::<&str>().unwrap();
    assert_eq!(*output, "expected panic");

    assert!(!flag.load(Ordering::SeqCst));
}

/// Similar to `replay_causality`, but with a schedule that also contains
/// random choice steps.
#[test]
fn replay_causality_with_random() {
    // The thread setup here is the same as in `replay_causality`, but thread
    // 3 is using the RNG rather than setting a Boolean flag.

    let result = panic::catch_unwind(|| {
        // Manually construct a schedule, to show explicitly the thread steps
        // and the random steps made for thread 3 (which are irrelevant to the
        // failure being replayed).
        let mut schedule = Schedule::new(0);
        schedule.push_task(0.into());
        schedule.push_task(0.into());
        schedule.push_task(1.into());
        schedule.push_task(1.into());
        schedule.push_task(0.into());
        schedule.push_task(0.into());
        schedule.push_task(3.into());
        schedule.push_random();
        schedule.push_random();
        schedule.push_random();
        schedule.push_task(2.into());
        schedule.push_task(0.into());
        schedule.push_task(1.into());
        schedule.push_task(2.into());
        schedule.push_task(2.into());

        let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
        scheduler.set_target_clock(&[2, 2, 1]);
        let mut config = Config::new();
        config.failure_persistence = FailurePersistence::None;
        let runner = Runner::new(scheduler, config);
        runner.run(move || {
            assert_eq!(me(), 0);
            let lock = Arc::new(Mutex::new(0usize));
            let lock_clone = Arc::clone(&lock);
            thread::spawn(move || {
                assert_eq!(me(), 1);
                *lock_clone.lock().unwrap() = 1;
            });
            thread::spawn(move || {
                assert_eq!(me(), 2);
                let guard = lock.lock().unwrap();
                assert!(*guard == 0, "expected panic");
                drop(guard);
            });
            thread::spawn(move || {
                use shuttle::rand::Rng;
                assert_eq!(me(), 3);
                let mut thread_rng = shuttle::rand::thread_rng();
                thread_rng.gen::<u64>();
                thread_rng.gen::<u64>();
                thread_rng.gen::<u64>();
            });
        });
    })
    .expect_err("test should panic");
    let output = result.downcast::<&str>().unwrap();
    assert_eq!(*output, "expected panic");
}
