//! This module contains the logic for printing and persisting enough failure information when a
//! test panics to allow the failure to be replayed.
//!
//! There are two points at which a failure can be reported. The runtime calls `persist_failure` once
//! the panic has finished unwinding, which is the default and yields the most complete schedule, since
//! it also covers the scheduling decisions the unwind needed. A custom panic hook
//! (`init_panic_hook`) can additionally report at the moment of the panic, which is the only thing
//! that gets a schedule out when a second panic during unwinding aborts the process; that is opt-in
//! via [`crate::config::Config::eager_failure_reports`], because it means a failure reports twice.
//!
//! When both fire, reporting is grow-only: each report must be longer than the last, so each
//! supersedes it and the same schedule is never reported twice. See `ActiveExecution::reported_steps`
//! for why that is the behaviour we want rather than reporting only the first.
//!
//! The hook is installed once per process but has to serve every `Runner`, so it cannot capture any
//! state of its own. `begin_execution` records which execution is running on the current thread, and
//! the hook reads that; see `ActiveExecution`.
use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::Once;

use crate::config::{Config, FailurePersistence};
use crate::runtime::execution::{CurrentSchedule, ExecutionState};
use crate::scheduler::serialization::serialize_schedule;

/// The execution currently running on this thread, for the benefit of the panic hook.
///
/// The hook is installed once per process, so it cannot capture a `Config`: the one it captured would
/// belong to whichever `Runner` happened to run first, and every later `Runner`'s
/// [`FailurePersistence`] setting would be ignored. Instead it reads the config of the execution that
/// is actually running.
///
/// `None` means no execution is in progress, in which case the hook stays quiet. That matters because
/// the hook also fires for the panic Shuttle itself raises to fail the test, and for any panic in the
/// surrounding test code once the execution is over, at which point there is no schedule left to talk
/// about.
struct ActiveExecution {
    config: Config,

    /// Number of steps in the longest schedule reported for this execution so far, if any.
    ///
    /// Only relevant when [`crate::config::Config::eager_failure_reports`] is set, since that is when
    /// a failure reports more than once. Reporting is then grow-only rather than once-only, and the
    /// length is what makes that decision. Two cases depend on it:
    ///
    /// * A panic the test catches itself still runs the hook. If that claimed a one-shot report, the
    ///   real failure later on would go unreported.
    /// * With the default [`crate::config::UngracefulShutdownConfig`], scheduling continues while the
    ///   panic unwinds, so the schedule that reproduces the failure most faithfully is the last one
    ///   reported, not the first.
    ///
    /// Comparing lengths is what keeps the later, longer report from being a duplicate of the earlier
    /// one rather than a replacement for it.
    reported_steps: Option<usize>,

    /// The file this execution's schedule was last written to, if any.
    ///
    /// A longer schedule rewrites that file rather than creating a second one, so one failure leaves
    /// behind one file holding the most complete schedule, instead of a pile of prefixes of itself.
    reported_path: Option<PathBuf>,
}

thread_local! {
    static ACTIVE_EXECUTION: RefCell<Option<ActiveExecution>> = const { RefCell::new(None) };
}

/// Marks the end of an execution when dropped, so the panic hook goes quiet again.
///
/// This has to be a guard rather than an explicit call at the end of the execution, because a failing
/// execution leaves by unwinding. If the active execution outlived it, then the next panic on this
/// thread (the test harness reporting the failure, an unrelated `unwrap` in the surrounding test,
/// anything at all) would be reported as a Shuttle failure and would serialize the schedule of an
/// execution that has long since finished.
#[must_use = "the execution is only marked as running for as long as this guard is alive"]
pub struct ExecutionGuard;

impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        ACTIVE_EXECUTION.with(|active| *active.borrow_mut() = None);
    }
}

/// Called at the start of each execution. Also clears what the previous execution reported, so that a
/// failure in one execution cannot suppress the report for a failure in a later one, and so that a
/// later execution does not rewrite an earlier one's schedule file.
pub fn begin_execution(config: &Config) -> ExecutionGuard {
    ACTIVE_EXECUTION.with(|active| {
        *active.borrow_mut() = Some(ActiveExecution {
            config: config.clone(),
            reported_steps: None,
            reported_path: None,
        });
    });
    ExecutionGuard
}

/// The config of the execution running on this thread, if there is one.
///
/// Cloned rather than borrowed, because the caller goes on to read the schedule and print, either of
/// which could panic and re-enter this module while a `RefCell` borrow was still open.
fn active_config() -> Option<Config> {
    ACTIVE_EXECUTION.with(|active| active.borrow().as_ref().map(|active| active.config.clone()))
}

/// Whether a schedule of `steps` steps is worth reporting: only if no execution has reported yet, or
/// if this schedule is longer than what was already reported for it.
fn should_report(steps: usize) -> bool {
    ACTIVE_EXECUTION.with(|active| match active.borrow().as_ref() {
        Some(active) => active.reported_steps.is_none_or(|reported| steps > reported),
        // Not inside a Shuttle execution, so there is no schedule to report.
        None => false,
    })
}

/// Record what was just reported. Called *after* reporting succeeds, so that a panic part-way through
/// does not leave the schedule marked as reported when it was not.
fn note_reported(steps: usize, path: Option<PathBuf>) {
    ACTIVE_EXECUTION.with(|active| {
        if let Some(active) = active.borrow_mut().as_mut() {
            active.reported_steps = Some(steps);
            active.reported_path = path;
        }
    });
}

/// The file this execution last wrote its schedule to, if any.
fn reported_path() -> Option<PathBuf> {
    ACTIVE_EXECUTION.with(|active| active.borrow().as_ref().and_then(|active| active.reported_path.clone()))
}

/// Report the failing schedule from the panic hook, using the running execution's config.
fn persist_failure_from_hook() {
    let Some(config) = active_config() else {
        return;
    };
    // Off by default: the runtime reports once the panic has unwound, which yields one schedule rather
    // than two and a schedule that covers the unwind. See `Config::eager_failure_reports` for when
    // reporting from here instead is worth it.
    if !config.eager_failure_reports {
        return;
    }
    // Checked before announcing anything, so that a panic which adds nothing to what has already been
    // reported stays silent instead of printing a header with no schedule under it.
    if !should_report(CurrentSchedule::len()) {
        return;
    }

    eprintln!("Task failed, serializing schedule");
    eprintln!("test panicked in task '{}'", ExecutionState::failing_task());
    persist_failure_inner(&config);
}

/// Persist (to stderr or to file) a message describing how to replay a failing schedule.
pub fn persist_failure(config: &Config) {
    persist_failure_inner(config);
}

fn persist_failure_inner(config: &Config) {
    let schedule = CurrentSchedule::get_schedule();
    let steps = schedule.len();
    if !should_report(steps) {
        return;
    }

    match &config.failure_persistence {
        FailurePersistence::None => {}
        FailurePersistence::File(directory) => {
            let serialized_schedule = serialize_schedule(&schedule);

            // Try to persist to a file, but fall through to stderr if that fails for some reason
            match persist_failure_to_file(&serialized_schedule, directory.as_ref(), reported_path()) {
                Ok(path) => {
                    eprintln!("failing schedule persisted to file: {}\npass that path to `shuttle::replay_from_file` to replay the failure", path.display());
                    note_reported(steps, Some(path));
                }
                Err(e) => {
                    eprintln!("failed to persist schedule to file (error: {e}), falling back to printing the schedule");
                    eprintln!(
                        "failing schedule:\n\"\n{serialized_schedule}\n\"\npass that string to `shuttle::replay` to replay the failure"
                    );
                    note_reported(steps, None);
                }
            }
        }
        FailurePersistence::Print => {
            let serialized_schedule = serialize_schedule(&schedule);
            // Say so when this supersedes an earlier block, because otherwise it is not obvious which
            // of two schedules in the output is the one to copy.
            let note = if steps_already_reported() {
                "\nthis schedule supersedes the one printed above, which stopped at the panic"
            } else {
                ""
            };
            eprintln!(
                "failing schedule:\n\"\n{serialized_schedule}\n\"\npass that string to `shuttle::replay` to replay the failure{note}"
            );
            note_reported(steps, None);
        }
    }
}

/// Whether anything has been reported for this execution yet.
fn steps_already_reported() -> bool {
    ACTIVE_EXECUTION.with(|active| {
        active
            .borrow()
            .as_ref()
            .is_some_and(|active| active.reported_steps.is_some())
    })
}

/// Persist the given serialized schedule to a file and return the file's path. The file will be
/// placed in the current directory unless `destination` says otherwise.
///
/// `reuse` is the path this execution already wrote to, if any. Reporting is grow-only, so a second
/// call for the same execution carries a longer schedule that supersedes what is in that file; it is
/// rewritten in place rather than joined by a second file holding a prefix of the same schedule.
fn persist_failure_to_file(
    serialized_schedule: &str,
    destination: Option<&PathBuf>,
    reuse: Option<PathBuf>,
) -> std::io::Result<PathBuf> {
    if let Some(path) = reuse {
        let mut file = OpenOptions::new().write(true).truncate(true).open(&path)?;
        file.write_all(serialized_schedule.as_bytes())?;
        return Ok(path);
    }

    // Try to find the first usable filename. This is quadratic but we don't expect a ton of
    // conflicts here.
    let mut i = 0;
    let dir = if let Some(dir) = destination {
        dir.clone()
    } else {
        std::env::current_dir()?
    };
    let (path, mut file) = loop {
        let path = dir.clone().join(Path::new(&format!("schedule{i:03}.txt")));
        // `create_new` does the existence check and creation atomically, so this loop ensures that
        // two concurrent tests won't try to persist to the same file.
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => break (path, file),
            Err(e) => {
                if e.kind() != ErrorKind::AlreadyExists {
                    return Err(e);
                }
            }
        }
        i += 1;
    };
    file.write_all(serialized_schedule.as_bytes())?;
    path.canonicalize()
}

/// Set up a panic hook that will try to print the current schedule to stderr so that the failure
/// can be replayed. Returns a guard that will disarm the panic hook when dropped.
///
/// See the module documentation for more details on how this method fits into the failure reporting
/// story.
pub fn init_panic_hook() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let original_hook = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            persist_failure_from_hook();
            original_hook(panic_info);
        }));
    });
}
