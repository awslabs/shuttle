//! The clock under Shuttle: that it starts over in every execution, that it replays, and that the
//! parts that do not read it work anywhere.

mod common;

use common::{START, offset, offsets_of_eight_readings, per_execution, readings_from_threads, start};
use shuttle::scheduler::{DfsScheduler, UncontrolledNondeterminismCheckScheduler};
use shuttle::sync::{Arc, Mutex};
use shuttle::{Config, Runner, thread};
use shuttle_engine::runtime::execution::CurrentSchedule;
use shuttle_engine::scheduler::ScheduleStep;
use shuttle_engine::scheduler::serialization::serialize_schedule;
use shuttle_time_inner::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::collections::HashSet;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn reading_the_clock_is_not_a_scheduling_point() {
    shuttle::check_random(
        || {
            let before = CurrentSchedule::get_schedule().steps.len();
            for _ in 0..10 {
                let _ = Instant::now();
                let _ = SystemTime::now();
            }
            let steps = CurrentSchedule::get_schedule().steps;
            // The twenty readings added twenty random draws to the schedule and no context switch,
            // so Shuttle replays the readings but does not run another task around them.
            let added = &steps[before..];
            assert_eq!(added.len(), 20);
            assert!(added.iter().all(|step| *step == ScheduleStep::Random), "{added:?}");
        },
        20,
    );
}

#[test]
fn the_clock_starts_over_in_every_execution() {
    let pairs = per_execution(|| (SystemTime::now(), SystemTime::now()), 1, 200);

    // The first reading of an execution is the start. A counter that carried over from the previous
    // execution would read later.
    assert!(pairs.iter().all(|(first, _)| *first == start()));

    // The second reading is zero or one nanosecond after it, and over 200 executions both happen.
    let advances = pairs
        .iter()
        .map(|(first, second)| second.duration_since(*first).unwrap().as_nanos())
        .collect::<HashSet<_>>();
    assert_eq!(advances, HashSet::from([0, 1]));
}

#[test]
fn the_same_seed_gives_the_same_readings() {
    assert_eq!(
        per_execution(offsets_of_eight_readings, 1, 20),
        per_execution(offsets_of_eight_readings, 1, 20)
    );
}

#[test]
fn a_different_seed_gives_different_readings() {
    assert_ne!(
        per_execution(offsets_of_eight_readings, 1, 20),
        per_execution(offsets_of_eight_readings, 2, 20)
    );
}

#[test]
fn executions_of_one_run_differ_from_each_other() {
    let executions = per_execution(offsets_of_eight_readings, 1, 20);
    assert!(executions.iter().collect::<HashSet<_>>().len() > 1);
}

#[test]
fn the_two_clocks_read_the_same_counter() {
    // Three `Instant` readings, then a `SystemTime` one. The last is as far from the start of the
    // execution as those three readings advanced the counter, which is more than zero in some
    // executions. Separate counters would always put it at the start.
    let offsets = per_execution(
        || {
            for _ in 0..3 {
                let _ = Instant::now();
            }
            offset(SystemTime::now())
        },
        1,
        200,
    );
    assert!(offsets.iter().any(|offset| *offset > Duration::ZERO));
    assert!(offsets.iter().all(|offset| *offset <= Duration::from_nanos(3)));
}

#[test]
fn readings_are_ordered_across_tasks() {
    shuttle::check_random(
        || {
            let readings = readings_from_threads(4, 3);
            assert!(readings.windows(2).all(|pair| pair[0] <= pair[1]), "{readings:?}");
            assert!(readings.last().unwrap().duration_since(readings[0]) <= Duration::from_nanos(11));
        },
        100,
    );
}

#[test]
fn under_dfs_each_reading_is_one_nanosecond_after_the_last() {
    shuttle::check_dfs(
        || {
            let offsets = (0..8).map(|_| offset(SystemTime::now()).as_nanos()).collect::<Vec<_>>();
            assert_eq!(offsets, (0..8).collect::<Vec<_>>());
            // The two clocks still read the same counter.
            let _ = Instant::now();
            assert_eq!(offset(SystemTime::now()), Duration::from_nanos(9));
        },
        None,
    );
}

#[test]
fn under_dfs_reading_the_clock_adds_nothing_to_the_schedule() {
    shuttle::check_dfs(
        || {
            let before = CurrentSchedule::len();
            for _ in 0..10 {
                let _ = Instant::now();
                let _ = SystemTime::now();
            }
            assert_eq!(CurrentSchedule::len(), before);
        },
        None,
    );
}

/// How many executions `check_dfs` explores for two threads that each take a lock twice, and read
/// the clock under it each time if `read`.
fn dfs_executions(read: bool) -> usize {
    let executions = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = executions.clone();
    shuttle::check_dfs(
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            if read {
                // In whichever order the threads took the lock, the readings are one nanosecond
                // apart, in that order.
                let readings = readings_from_threads(2, 2);
                let offsets = readings
                    .iter()
                    .map(|reading| reading.duration_since(readings[0]).as_nanos())
                    .collect::<Vec<_>>();
                assert_eq!(offsets, [0, 1, 2, 3]);
            } else {
                let lock = Arc::new(Mutex::new(()));
                let threads = (0..2)
                    .map(|_| {
                        let lock = lock.clone();
                        thread::spawn(move || {
                            for _ in 0..2 {
                                drop(lock.lock().unwrap());
                            }
                        })
                    })
                    .collect::<Vec<_>>();
                for thread in threads {
                    thread.join().unwrap();
                }
            }
        },
        None,
    );
    executions.load(Ordering::SeqCst)
}

#[test]
fn under_dfs_reading_the_clock_adds_no_schedules() {
    let without = dfs_executions(false);
    assert!(without > 1, "{without}");
    assert_eq!(dfs_executions(true), without);
}

#[test]
fn dfs_with_random_data_takes_the_zero_or_one_step() {
    // The step follows the scheduler, not the search: a `DfsScheduler` that provides random data
    // gets the drawn step, which its fixed data source makes the same in every execution.
    let advances = std::sync::Arc::new(std::sync::Mutex::new(HashSet::new()));
    let sink = advances.clone();
    Runner::new(DfsScheduler::new(None, true), Config::new()).run(move || {
        let before = CurrentSchedule::len();
        let offsets = (0..16)
            .map(|_| offset(SystemTime::now()).as_nanos())
            .collect::<Vec<_>>();
        assert_eq!(CurrentSchedule::len(), before + 16, "one draw per reading");
        sink.lock()
            .unwrap()
            .extend(offsets.windows(2).map(|pair| pair[1] - pair[0]));
    });
    assert_eq!(*advances.lock().unwrap(), HashSet::from([0, 1]));
}

#[test]
fn under_pct_each_reading_moves_the_clock_by_zero_or_one_nanosecond() {
    let advances = std::sync::Arc::new(std::sync::Mutex::new(HashSet::new()));
    let sink = advances.clone();
    shuttle::check_pct(
        move || {
            // PCT insists on a test with a scheduling choice.
            thread::spawn(|| {});
            let first = SystemTime::now();
            let second = SystemTime::now();
            sink.lock()
                .unwrap()
                .insert(second.duration_since(first).unwrap().as_nanos());
        },
        200,
        2,
    );
    assert_eq!(*advances.lock().unwrap(), HashSet::from([0, 1]));
}

#[test]
fn a_failing_dfs_schedule_replays_with_the_same_readings() {
    // The readings of the latest execution, with the thread that took each, and its schedule.
    static LOG: std::sync::Mutex<(Vec<(usize, Instant)>, String)> = std::sync::Mutex::new((Vec::new(), String::new()));

    // Fails in the schedules where the second thread reads the clock first.
    fn first_thread_reads_first() {
        let readings = Arc::new(Mutex::new(Vec::new()));
        let threads = (0..2)
            .map(|index| {
                let readings = readings.clone();
                thread::spawn(move || {
                    let mut readings = readings.lock().unwrap();
                    readings.push((index, Instant::now()));
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
        let readings = readings.lock().unwrap().clone();
        *LOG.lock().unwrap() = (readings.clone(), serialize_schedule(&CurrentSchedule::get_schedule()));
        assert_eq!(readings[0].0, 0, "the second thread read the clock first");
    }

    panic::catch_unwind(AssertUnwindSafe(|| shuttle::check_dfs(first_thread_reads_first, None)))
        .expect_err("test should fail");
    let (failing, schedule) = LOG.lock().unwrap().clone();
    assert_eq!(failing[1].1.duration_since(failing[0].1), Duration::from_nanos(1));

    LOG.lock().unwrap().0.clear();
    panic::catch_unwind(|| shuttle::replay(first_thread_reads_first, &schedule)).expect_err("replay should fail");
    assert_eq!(LOG.lock().unwrap().0, failing);
}

#[test]
fn under_dfs_a_schedule_that_depends_on_the_clock_replays() {
    let scheduler = UncontrolledNondeterminismCheckScheduler::new(DfsScheduler::new(None, false));
    Runner::new(scheduler, Config::new()).run(|| {
        let first = Instant::now();
        let second = Instant::now();
        spawn_if(second > first);
    });
}

#[test]
fn instant_subtraction_is_bounded_by_the_base() {
    shuttle::check_random(
        || {
            let now = Instant::now();
            // The base is far enough back to subtract any plausible duration from a reading.
            assert!(now.checked_sub(Duration::from_secs(50 * 365 * 24 * 60 * 60)).is_some());
            // Further back than the base cannot be represented, where `std::time::Instant` on Linux
            // can represent a time before the machine booted.
            assert_eq!(now.checked_sub(Duration::from_secs(60 * 365 * 24 * 60 * 60)), None);
        },
        1,
    );
}

/// Takes a different path through the schedule depending on `taken`, so that a schedule recorded
/// with one value does not replay with the other.
fn spawn_if(taken: bool) {
    if taken {
        shuttle::thread::spawn(|| {}).join().unwrap();
    }
}

#[test]
fn a_schedule_that_depends_on_the_clock_replays() {
    shuttle::check_uncontrolled_nondeterminism(
        || {
            let first = Instant::now();
            let second = Instant::now();
            spawn_if(second > first);
        },
        50,
    );
}

#[test]
#[should_panic(expected = "possible nondeterminism")]
fn the_same_schedule_on_the_real_clock_does_not_replay() {
    // The control for the test above: with the operating system's clock the branch is outside
    // Shuttle's control, and the checker reports it.
    shuttle::check_uncontrolled_nondeterminism(
        || {
            let first = std::time::Instant::now();
            let second = std::time::Instant::now();
            spawn_if(second.duration_since(first).subsec_nanos().is_multiple_of(2));
        },
        50,
    );
}

#[test]
#[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
fn instant_now_outside_of_shuttle_panics() {
    Instant::now();
}

#[test]
#[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
fn system_time_now_outside_of_shuttle_panics() {
    SystemTime::now();
}

#[test]
fn everything_but_now_works_outside_of_shuttle() {
    // The Unix epoch is still 1970-01-01T00:00:00Z, and the clock's start is in 2026.
    assert_eq!(UNIX_EPOCH, SystemTime::UNIX_EPOCH);
    assert_eq!(start().duration_since(UNIX_EPOCH).unwrap(), START);

    let later = UNIX_EPOCH + Duration::from_secs(10);
    assert_eq!(later.duration_since(UNIX_EPOCH).unwrap(), Duration::from_secs(10));
    assert_eq!(later - Duration::from_secs(10), UNIX_EPOCH);
    assert_eq!(
        UNIX_EPOCH.duration_since(later).unwrap_err().duration(),
        Duration::from_secs(10)
    );
    assert!(UNIX_EPOCH < later);

    // `SystemTime` converts both ways with `std::time`'s.
    let handed_over: std::time::SystemTime = later.into();
    assert_eq!(handed_over, std::time::UNIX_EPOCH + Duration::from_secs(10));
    assert_eq!(SystemTime::from(handed_over), later);
}
