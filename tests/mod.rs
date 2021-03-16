#![deny(warnings)]

mod asynch;
mod basic;
mod data;
mod demo;

use shuttle::scheduler::{ReplayScheduler, Scheduler};
use shuttle::{replay_from_file, Config, FailurePersistence, Runner};
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
            config.failure_persistence = FailurePersistence::File {
                directory: Some(tempdir_path),
            };
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
        let string_regex = Regex::new("failing schedule: \"([0-9a-f]+)\"").unwrap();
        let captures = string_regex.captures(output.as_ref().as_str())?;
        Some(captures.get(1)?.as_str().to_string())
    }
}
