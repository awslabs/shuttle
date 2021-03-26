use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::scheduler::serialization::serialize_schedule;
use crate::scheduler::Schedule;
use crate::{Config, FailurePersistence};

/// Produce a message describing how to replay a failing schedule.
pub fn persist_failure(schedule: &Schedule, message: String, config: &Config) -> String {
    let serialized_schedule = serialize_schedule(schedule);
    // Try to persist to a file, but fall through to stdout if that fails for some reason
    if let FailurePersistence::File { directory } = &config.failure_persistence {
        match persist_failure_to_file(&serialized_schedule, directory.as_ref()) {
            Ok(path) => return format!("{}\nfailing schedule persisted to file: {}\npass that path to `shuttle::replay_from_file` to replay the failure", message, path.display()),
            Err(e) => eprintln!("failed to persist schedule to file (error: {}), falling back to printing the schedule", e),
        }
    }
    format!(
        "{}\nfailing schedule: \"{}\"\npass that path to `shuttle::replay` to replay the failure",
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
