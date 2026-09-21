#![deny(warnings)]

mod advanced;
mod basic;
mod data;
mod demo;
mod future;

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}

use shuttle::scheduler::{RandomScheduler, ReplayScheduler, Scheduler};
use shuttle::{replay_from_file, Config, FailurePersistence, Runner, ScheduleEncoding, ScheduleTextEncoding};
use std::any::Any;
use std::panic::{self, RefUnwindSafe, UnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The message of a caught panic, whichever payload type it happened to use.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "<non-string panic payload>".to_owned())
}

/// Run a Shuttle test that is expected to fail, persisting the failing schedule into `dir`, and
/// return the panic message together with the path of the schedule that was written.
///
/// Reading the schedule from a file rather than scraping it out of the panic message is deliberate.
/// Shuttle reports the failing schedule from its panic hook, at the moment of the panic, so that a
/// schedule is still reported if a second panic while unwinding aborts the process. That report goes
/// to stderr (or a file), never into the panic payload.
fn run_expecting_failure<F, S>(test_func: F, scheduler: S, config: Config, dir: &Path) -> (String, PathBuf)
where
    F: Fn() + Send + Sync + UnwindSafe + 'static,
    S: Scheduler + UnwindSafe + 'static,
{
    let payload = {
        let mut config = config;
        config.failure_persistence = FailurePersistence::File(Some(dir.to_path_buf()));
        panic::catch_unwind(move || Runner::new(scheduler, config).run(test_func)).expect_err("test should panic")
    };

    let mut schedules = std::fs::read_dir(dir)
        .expect("could not read schedule directory")
        .map(|entry| entry.expect("bad directory entry").path())
        .collect::<Vec<_>>();
    schedules.sort();
    // One failure means one file. A failing execution reaches the failure reporting path twice, from
    // the panic hook and again from the runtime once the panic has unwound, and the second schedule is
    // a longer version of the first. The longer one rewrites the file rather than adding a second, so
    // what is left behind is one file holding the most complete schedule.
    assert_eq!(
        schedules.len(),
        1,
        "expected exactly one persisted schedule, got {schedules:?}"
    );

    (panic_message(payload.as_ref()), schedules.pop().unwrap())
}

fn read_schedule(path: &Path) -> String {
    let schedule = std::fs::read_to_string(path).expect("could not read schedule file");
    assert!(!schedule.trim().is_empty(), "persisted an empty schedule");
    schedule
}

/// Validates that schedule replay works by running a test, expecting it to fail, and then replaying
/// the schedule it persisted. The replay must reproduce the same panic and record the same schedule.
fn check_replay_roundtrip<F, S>(test_func: F, scheduler: S)
where
    F: Fn() + Send + Sync + RefUnwindSafe + 'static,
    S: Scheduler + UnwindSafe + 'static,
{
    let test_func = Arc::new(test_func);

    let dir = tempfile::tempdir().expect("could not create tempdir");
    let (output, path) = {
        let test_func = test_func.clone();
        run_expecting_failure(move || test_func(), scheduler, Config::new(), dir.path())
    };
    let schedule = read_schedule(&path);

    // Note that this replay does not set `allow_incomplete`: a schedule Shuttle persisted has to be
    // enough to reproduce the failure on its own.
    let replay_dir = tempfile::tempdir().expect("could not create tempdir");
    let (new_output, new_path) = run_expecting_failure(
        move || test_func(),
        ReplayScheduler::new_from_encoded(&schedule),
        Config::new(),
        replay_dir.path(),
    );

    assert_eq!(read_schedule(&new_path), schedule);
    // This might be too strong a check, but seems reasonable: the panics should be identical
    assert_eq!(new_output, output);
}

/// As [`check_replay_roundtrip`], but loading the schedule back through the file-based entry points,
/// including the [`replay_from_file`] convenience wrapper.
fn check_replay_roundtrip_file<F, S>(test_func: F, scheduler: S)
where
    F: Fn() + Send + Sync + RefUnwindSafe + 'static,
    S: Scheduler + UnwindSafe + 'static,
{
    let test_func = Arc::new(test_func);

    let dir = tempfile::tempdir().expect("could not create tempdir");
    let (output, path) = {
        let test_func = test_func.clone();
        run_expecting_failure(move || test_func(), scheduler, Config::new(), dir.path())
    };
    let schedule = read_schedule(&path);

    let replay_dir = tempfile::tempdir().expect("could not create tempdir");
    let (new_output, new_path) = {
        let test_func = test_func.clone();
        run_expecting_failure(
            move || test_func(),
            ReplayScheduler::new_from_file(&path).expect("could not load schedule from file"),
            Config::new(),
            replay_dir.path(),
        )
    };
    assert_eq!(read_schedule(&new_path), schedule);
    assert_eq!(new_output, output);

    // `replay_from_file` keeps the default `FailurePersistence`, so it prints its schedule rather
    // than writing a file; all we can check here is that it reproduces the same failure.
    let wrapper_output =
        panic::catch_unwind(move || replay_from_file(move || test_func(), path)).expect_err("replay should panic");
    assert_eq!(panic_message(wrapper_output.as_ref()), output);
}

/// Validates that replaying from a seed is deterministic, by running a failing seed found by a random
/// scheduler for one iteration and checking that it persists exactly the expected schedule.
///
/// The expected schedules are pinned in the legacy fixed-width hex encoding. That keeps the literals
/// readable ASCII, and doubles as a check that Shuttle can still write the old format on request.
fn check_replay_from_seed_match_schedule<F>(test_func: F, seed: u64, expected_schedule: &str)
where
    F: Fn() + Send + Sync + UnwindSafe + 'static,
{
    let mut config = Config::new();
    config.schedule_encoding = ScheduleEncoding::FixedWidth;
    config.schedule_text_encoding = ScheduleTextEncoding::Hex;

    let dir = tempfile::tempdir().expect("could not create tempdir");
    let (_, path) = run_expecting_failure(test_func, RandomScheduler::new_from_seed(seed, 1), config, dir.path());

    // Compared with whitespace stripped, because what this test pins is the sequence of scheduling
    // decisions, not the width a schedule happens to be wrapped at. Deserialization ignores
    // whitespace too, so a difference in wrapping is not a difference in schedule.
    let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    assert_eq!(strip(&read_schedule(&path)), strip(expected_schedule));
}
