//! This module contains the logic for printing and persisting enough failure information when a
//! test panics to allow the failure to be replayed.
//!
//! The core idea is that we install a custom panic hook (`init_panic_hook`) that runs when a thread
//! panics. That hook tries to print information about the failing schedule by calling
//! `persist_failure`.
//!
//! The complexity in this logic comes from a few requirements beyond the simple panic hook:
//! 1. If a panic occurs within `ExecutionState`, the panic hook might not be able to access the
//!    execution state to retrieve the failing schedule, so we need to be careful about accessing it
//!    and try to recover from this problem when the panic is later caught in
//!    `ExecutionState::step`. We try wherever possible to avoid panicking in this state, but if it
//!    does happen we want to get useful output and not crash.
//! 2. In addition to simply printing the failing schedule, we want to include it in the panic
//!    payload wherever possible, so that we can parse it back out in tests (essentially we are
//!    reinventing try/catch, which is usually an anti-pattern in Rust, but here we don't have
//!    control over the user code that panics). That means sometimes we end up calling
//!    `persist_schedule` twice -- once in the panic hook (where we can't modify the panic payload)
//!    and again when we catch the panic (where we can modify the payload). We don't want to print
//!    the schedule twice, so we keep track of whether the info has already been printed.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};

use crate::runtime::execution::ExecutionState;
use crate::scheduler::serialization::serialize_schedule;
use crate::scheduler::Schedule;
use crate::{Config, FailurePersistence};

/// Produce a message describing how to replay a failing schedule.
///
/// If `print_if_fresh` is true, the message will also be printed to stderr if this is the first
/// time `persist_failure` has been called.
pub fn persist_failure(schedule: &Schedule, message: String, config: &Config, print_if_fresh: bool) -> String {
    // Disarm the panic hook so that we don't print the failure twice
    if let PanicHookState::Persisted(persisted_message) = PANIC_HOOK.with(|lock| lock.lock().unwrap().clone()) {
        return persisted_message;
    }

    let persisted_message = persist_failure_inner(schedule, message, config);
    PANIC_HOOK.with(|lock| *lock.lock().unwrap() = PanicHookState::Persisted(persisted_message.clone()));
    if print_if_fresh {
        eprintln!("{}", persisted_message);
    }
    persisted_message
}

/// Produce a message describing how to replay a failing schedule that ended on a given task.
pub fn persist_task_failure(schedule: &Schedule, task_name: String, config: &Config, print_if_fresh: bool) -> String {
    persist_failure(
        schedule,
        format!("test panicked in task '{}'", task_name),
        config,
        print_if_fresh,
    )
}

fn persist_failure_inner(schedule: &Schedule, message: String, config: &Config) -> String {
    if config.failure_persistence == FailurePersistence::None {
        return message;
    }

    let serialized_schedule = serialize_schedule(schedule);
    // Try to persist to a file, but fall through to stdout if that fails for some reason
    if let FailurePersistence::File(directory) = &config.failure_persistence {
        match persist_failure_to_file(&serialized_schedule, directory.as_ref()) {
            Ok(path) => return format!("{}\nfailing schedule persisted to file: {}\npass that path to `shuttle::replay_from_file` to replay the failure", message, path.display()),
            Err(e) => eprintln!("failed to persist schedule to file (error: {}), falling back to printing the schedule", e),
        }
    }
    format!(
        "{}\nfailing schedule:\n\"\n{}\n\"\npass that string to `shuttle::replay` to replay the failure",
        message, serialized_schedule
    )
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
        let path = dir.clone().join(Path::new(&format!("schedule{:03}.txt", i)));
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

#[derive(Debug, Clone)]
enum PanicHookState {
    Disarmed,
    Armed(Config),
    Persisted(String),
}

thread_local! {
    static PANIC_HOOK: Mutex<PanicHookState> = const { Mutex::new(PanicHookState::Disarmed) };
}

/// A guard that disarms the panic hook when dropped
#[derive(Debug)]
#[non_exhaustive]
pub struct PanicHookGuard;

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        PANIC_HOOK.with(|lock| *lock.lock().unwrap() = PanicHookState::Disarmed);
    }
}

/// Set up a panic hook that will try to print the current schedule to stderr so that the failure
/// can be replayed. Returns a guard that will disarm the panic hook when dropped.
///
/// See the module documentation for more details on how this method fits into the failure reporting
/// story.
#[must_use = "the panic hook will be disarmed when the returned guard is dropped"]
pub fn init_panic_hook(config: Config) -> PanicHookGuard {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let original_hook = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            let state = PANIC_HOOK.with(|lock| std::mem::replace(&mut *lock.lock().unwrap(), PanicHookState::Disarmed));
            // The hook is armed if this is the first time it's fired
            if let PanicHookState::Armed(config) = state {
                // We might not be able to get the info we need (e.g., if we panic while borrowing
                // ExecutionState)
                if let Some((name, schedule)) = ExecutionState::failure_info() {
                    persist_task_failure(&schedule, name, &config, true);
                }
            }
            original_hook(panic_info);
        }));
    });

    PANIC_HOOK.with(|lock| *lock.lock().unwrap() = PanicHookState::Armed(config));

    PanicHookGuard
}
