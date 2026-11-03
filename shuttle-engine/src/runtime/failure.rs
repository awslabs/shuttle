//! This module contains the logic for printing and persisting enough failure information when a
//! test panics to allow the failure to be replayed.
//!
//! The core idea is that we install a custom panic hook (`init_panic_hook`) that runs when a thread
//! panics. That hook tries to print information about the failing schedule by calling
//! `persist_failure`.
use std::cell::Cell;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::Once;

use crate::config::{Config, FailurePersistence};
use crate::runtime::execution::{CurrentSchedule, ExecutionState};
use crate::scheduler::serialization::serialize_schedule;

// When we last persisted a schedule. Used so that we don't persist the same schedule twice.
thread_local! {
    static SCHEDULE_PERSISTED_AT: Cell<usize> = const { Cell::new(0) };
    // Whether the running test has a time model other than the default, so that its failures replay only under it.
    static CUSTOM_TIME_MODEL: Cell<bool> = const { Cell::new(false) };
}

/// Records whether the running test has a custom time model, for as long as it lives.
pub(crate) struct CustomTimeModelGuard(bool);

impl CustomTimeModelGuard {
    pub(crate) fn new(custom: bool) -> Self {
        Self(CUSTOM_TIME_MODEL.replace(custom))
    }
}

impl Drop for CustomTimeModelGuard {
    fn drop(&mut self) {
        CUSTOM_TIME_MODEL.set(self.0);
    }
}

/// How to replay a schedule printed as a string, and one persisted to a file.
fn replay_instructions() -> (&'static str, &'static str) {
    if CUSTOM_TIME_MODEL.get() {
        (
            "pass that string and the same time model to `shuttle::replay_with_time_model` to replay the failure",
            "pass that path and the same time model to `shuttle::replay_from_file_with_time_model` to replay the failure",
        )
    } else {
        (
            "pass that string to `shuttle::replay` to replay the failure",
            "pass that path to `shuttle::replay_from_file` to replay the failure",
        )
    }
}

/// Persist (to stderr or to file) a message describing how to replay a failing schedule.
pub fn persist_failure(config: &Config) {
    // Don't serialize the same schedule twice.
    if SCHEDULE_PERSISTED_AT.get() == CurrentSchedule::len() {
        return;
    }

    let (replay_string, replay_file) = replay_instructions();
    match &config.failure_persistence {
        FailurePersistence::None => {}
        FailurePersistence::File(directory) => {
            let serialized_schedule = serialize_schedule(&CurrentSchedule::get_schedule());

            // Try to persist to a file, but fall through to stderr if that fails for some reason
            match persist_failure_to_file(&serialized_schedule, directory.as_ref()) {
                Ok(path) => eprintln!("failing schedule persisted to file: {}\n{replay_file}", path.display()),
                Err(e) => {
                    eprintln!("failed to persist schedule to file (error: {e}), falling back to printing the schedule");
                    eprintln!("failing schedule:\n\"\n{serialized_schedule}\n\"\n{replay_string}");
                }
            }
        }
        FailurePersistence::Print => {
            let serialized_schedule = serialize_schedule(&CurrentSchedule::get_schedule());
            eprintln!("failing schedule:\n\"\n{serialized_schedule}\n\"\n{replay_string}");
        }
    }

    SCHEDULE_PERSISTED_AT.set(CurrentSchedule::len());
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
pub fn init_panic_hook(config: Config) {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let original_hook = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            // Execution teardown ignores some of the panics it catches, which aren't to be reported
            // (see `ExecutionState::tear_down`).
            if ExecutionState::teardown_ignores_panic(panic_info.payload()) {
                return;
            }
            eprintln!("Task failed, serializing schedule");
            let task_name = ExecutionState::failing_task();
            eprintln!("test panicked in task '{task_name}'");
            persist_failure(&config);
            original_hook(panic_info);
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_instructions_name_the_functions_for_the_time_model() {
        assert!(replay_instructions().0.contains("`shuttle::replay`"));
        assert!(replay_instructions().1.contains("`shuttle::replay_from_file`"));
        {
            let _custom = CustomTimeModelGuard::new(true);
            let (string, file) = replay_instructions();
            assert!(string.contains("`shuttle::replay_with_time_model`"), "{string}");
            assert!(file.contains("`shuttle::replay_from_file_with_time_model`"), "{file}");
        }
        assert!(replay_instructions().0.contains("`shuttle::replay`"));
    }
}
