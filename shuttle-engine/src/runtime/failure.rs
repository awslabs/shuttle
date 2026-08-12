//! This module contains the logic for printing and persisting enough failure information when a
//! test panics to allow the failure to be replayed.
//!
//! The core idea is that we install a custom panic hook (`init_panic_hook`) that runs when a thread
//! panics. That hook tries to print information about the failing schedule by calling
//! `persist_failure`. Reporting from the hook, rather than after the panic has unwound, means a
//! schedule is still reported when a second panic during unwinding aborts the process.
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
use crate::scheduler::serialization::serialize_schedule_with;

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
    /// Whether this execution has already reported a failing schedule.
    ///
    /// A failing execution reaches `persist_failure` twice: from the panic hook at the moment of the
    /// panic, and again from the runtime once the panic has finished unwinding. The first schedule
    /// ends at the failure; the second additionally contains whatever scheduling decisions the unwind
    /// needed. Both replay the failure, so we report the first and suppress the second. Emitting two
    /// schedules for one failure leaves the reader guessing which to copy, and under
    /// `FailurePersistence::File` it writes two files per failure.
    persisted: bool,
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

/// Called at the start of each execution. Also clears the "already reported" flag, so that a failure
/// in one execution does not suppress the report for a failure in a later one.
pub fn begin_execution(config: &Config) -> ExecutionGuard {
    ACTIVE_EXECUTION.with(|active| {
        *active.borrow_mut() = Some(ActiveExecution {
            config: config.clone(),
            persisted: false,
        });
    });
    ExecutionGuard
}

/// Claim the right to report this execution's failure, returning false if it has already been
/// reported or if no execution is running.
fn claim_report() -> bool {
    ACTIVE_EXECUTION.with(|active| match &mut *active.borrow_mut() {
        Some(active) => !std::mem::replace(&mut active.persisted, true),
        None => false,
    })
}

/// Report the failing schedule from the panic hook, using the running execution's config.
fn persist_failure_from_hook() {
    // Cloning avoids holding the `RefCell` borrow across `persist_failure_inner`, which reads the
    // schedule and may itself panic.
    let config = ACTIVE_EXECUTION.with(|active| active.borrow().as_ref().map(|a| a.config.clone()));
    let Some(config) = config else {
        // Not inside a Shuttle execution, so there is no schedule to report.
        return;
    };
    if !claim_report() {
        return;
    }

    eprintln!("Task failed, serializing schedule");
    eprintln!("test panicked in task '{}'", ExecutionState::failing_task());
    persist_failure_inner(&config);
}

/// Persist (to stderr or to file) a message describing how to replay a failing schedule.
pub fn persist_failure(config: &Config) {
    if !claim_report() {
        return;
    }
    persist_failure_inner(config);
}

fn persist_failure_inner(config: &Config) {
    match &config.failure_persistence {
        FailurePersistence::None => {}
        FailurePersistence::File(directory) => {
            let serialized_schedule = serialize_schedule_with(
                &CurrentSchedule::get_schedule(),
                config.schedule_encoding,
                config.schedule_text_encoding,
            );

            // Try to persist to a file, but fall through to stderr if that fails for some reason
            match persist_failure_to_file(&serialized_schedule, directory.as_ref()) {
                Ok(path) => eprintln!("failing schedule persisted to file: {}\npass that path to `shuttle::replay_from_file` to replay the failure", path.display()),
                Err(e) => {
                    eprintln!("failed to persist schedule to file (error: {e}), falling back to printing the schedule");
                    eprintln!(
                        "failing schedule:\n\"\n{serialized_schedule}\n\"\npass that string to `shuttle::replay` to replay the failure"
                    );
                }
            }
        }
        FailurePersistence::Print => {
            let serialized_schedule = serialize_schedule_with(
                &CurrentSchedule::get_schedule(),
                config.schedule_encoding,
                config.schedule_text_encoding,
            );
            eprintln!(
                "failing schedule:\n\"\n{serialized_schedule}\n\"\npass that string to `shuttle::replay` to replay the failure"
            );
        }
    }
}

/// Persist the given serialized schedule to a file and return the new file's path. The file will be
/// placed in the current directory.
fn persist_failure_to_file(serialized_schedule: &str, destination: Option<&PathBuf>) -> std::io::Result<PathBuf> {
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
