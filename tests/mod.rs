#![deny(warnings)]

mod basic;
mod data;
mod demo;
mod future;

use shuttle::scheduler::{ReplayScheduler, Scheduler};
use shuttle::{check_random_with_seed, replay_from_file, Config, FailurePersistence, Runner};
use std::panic::{self, RefUnwindSafe, UnwindSafe};
use std::sync::Arc;

/// Validates that schedule replay works by running a test, expecting it to fail, and then parsing
/// and replaying the failing schedule from its output.
fn check_replay_roundtrip<F, S>(test_func: F, scheduler: S)
where
    F: Fn() + Send + Sync + RefUnwindSafe + 'static,
    S: Scheduler + UnwindSafe + 'static,
{
    let test_func = Arc::new(test_func);

    // Run the test that should fail and capture the schedule it prints
    let result = {
        let test_func = test_func.clone();
        panic::catch_unwind(move || {
            let mut config = Config::new();
            config.failure_persistence = FailurePersistence::Print;
            let runner = Runner::new(scheduler, config);
            runner.run(move || test_func())
        })
        .expect_err("test should panic")
    };
    let output = result.downcast::<String>().unwrap();
    let schedule = parse_schedule::from_stdout(&output).expect("output should contain a schedule");

    // Now replay that schedule and make sure it still fails and outputs the same schedule
    let result = {
        let schedule = schedule.clone();
        panic::catch_unwind(move || {
            let mut config = Config::new();
            config.failure_persistence = FailurePersistence::Print;
            let scheduler = ReplayScheduler::new_from_encoded(&schedule);
            let runner = Runner::new(scheduler, config);

            runner.run(move || test_func());
        })
        .expect_err("replay should panic")
    };
    let new_output = result.downcast::<String>().unwrap();
    let new_schedule = parse_schedule::from_stdout(&new_output).expect("output should contain a schedule");

    assert_eq!(new_schedule, schedule);
    // This might be too strong a check, but seems reasonable: the panics should be identical
    assert_eq!(new_output, output);
}

/// Validates that schedule replay works by running a test, expecting it to fail, and then parsing
/// and replaying the failing schedule from its output.
fn check_replay_roundtrip_file<F, S>(test_func: F, scheduler: S)
where
    F: Fn() + Send + Sync + RefUnwindSafe + 'static,
    S: Scheduler + UnwindSafe + 'static,
{
    let tempdir = tempfile::tempdir().expect("could not create tempdir");
    let test_func = Arc::new(test_func);

    // Run the test that should fail and capture the schedule it prints
    let result = {
        let test_func = test_func.clone();
        let tempdir_path = tempdir.path().to_path_buf();
        panic::catch_unwind(move || {
            let mut config = Config::new();
            config.failure_persistence = FailurePersistence::File(Some(tempdir_path));
            let runner = Runner::new(scheduler, config);
            runner.run(move || test_func())
        })
        .expect_err("test should panic")
    };
    let output = result.downcast::<String>().unwrap();
    let (schedule, schedule_file) = parse_schedule::from_file(&output).expect("output should contain a schedule");

    // Now replay that schedule and make sure it still fails and outputs the same schedule. We want
    // to test the `replay_from_file` function directly, so this time we'll default to printing the
    // schedule to stdout.
    let result = {
        panic::catch_unwind(move || replay_from_file(move || test_func(), schedule_file))
            .expect_err("replay should panic")
    };
    let new_output = result.downcast::<String>().unwrap();
    let new_schedule = parse_schedule::from_stdout(&new_output).expect("output should contain a schedule");

    assert_eq!(new_schedule, schedule);
    // Stronger `output == new_output` check doesn't hold here because we used different values of
    // `FailurePersistence`s for each test
}

/// Validates that the replay from seed functionality works by running a failing seed found by a random
/// scheduler for one iteration, expecting it to fail, comparing the new failing schedule against the
/// previously collected one, and checking the two schedules being identical.
fn check_replay_from_seed_match_schedule<F>(test_func: F, seed: u64, expected_schedule: &str)
where
    F: Fn() + Send + Sync + UnwindSafe + 'static,
{
    let result = {
        panic::catch_unwind(move || {
            check_random_with_seed(test_func, seed, 1);
        })
        .expect_err("replay should panic")
    };
    let output = result.downcast::<String>().unwrap();
    let schedule_from_replay = parse_schedule::from_stdout(&output).expect("output should contain a schedule");

    assert_eq!(schedule_from_replay, expected_schedule);
}

/// Helpers to parse schedules from different types of output (as determined by [`FailurePersistence`])
mod parse_schedule {
    use regex::Regex;
    use std::fs::OpenOptions;
    use std::io::Read;
    use std::path::PathBuf;

    pub(super) fn from_file<S: AsRef<String>>(output: S) -> Option<(String, PathBuf)> {
        let file_regex = Regex::new("persisted to file: (.*)").unwrap();
        let file_match = file_regex.captures(output.as_ref().as_str())?.get(1)?.as_str();
        let mut file = OpenOptions::new().read(true).open(file_match).ok()?;
        let mut schedule = String::new();
        file.read_to_string(&mut schedule).ok()?;
        Some((schedule, PathBuf::from(file_match)))
    }

    pub(super) fn from_stdout<S: AsRef<String>>(output: S) -> Option<String> {
        let mut schedule = String::new();
        let mut lines = output.as_ref().lines();
        for line in &mut lines {
            if line.eq("failing schedule:") {
                break;
            }
        }
        assert_eq!(lines.next().unwrap(), "\"");
        for line in lines {
            if line.eq("\"") {
                schedule.pop(); // trailing newline, if any
                return Some(schedule);
            }
            schedule.push_str(line);
            schedule.push('\n');
        }
        None
    }
}
