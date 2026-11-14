// Each test binary that includes this module uses some of it.
#![allow(dead_code)]

use shuttle::sync::{Arc, Mutex};
use shuttle::thread;
use shuttle_time_inner::time::{Instant, SystemTime, UNIX_EPOCH};
use std::time::Duration;

/// Where the clock starts in every execution, as a duration since the Unix epoch. The crate's own
/// constant is private, so the tests state it again.
pub const START: Duration = Duration::from_secs(1_767_225_600);

/// The first reading of `SystemTime::now()` in an execution.
pub fn start() -> SystemTime {
    UNIX_EPOCH + START
}

/// How far a reading is from the start of its execution.
pub fn offset(reading: SystemTime) -> Duration {
    reading
        .duration_since(start())
        .expect("a reading is at or after the start")
}

/// The readings of the whole execution, as nanoseconds after the start, in the order they were
/// taken.
pub fn offsets_of_eight_readings() -> Vec<u128> {
    (0..8).map(|_| offset(SystemTime::now()).as_nanos()).collect::<Vec<_>>()
}

/// Runs `body` once in each of `executions` executions of a random scheduler seeded with `seed`,
/// and returns what it returned in each of them.
pub fn per_execution<T: Clone + Send + 'static>(body: fn() -> T, seed: u64, executions: usize) -> Vec<T> {
    // A `std::sync::Mutex` on purpose: it holds what the executions produced, and so has to outlive
    // them.
    let collected = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = collected.clone();
    shuttle::check_random_with_seed(move || sink.lock().unwrap().push(body()), seed, executions);
    let readings = collected.lock().unwrap().clone();
    assert_eq!(readings.len(), executions, "every execution ran the body once");
    readings
}

/// Spawns `num_threads` threads that each read the clock `reads` times, and returns the readings in
/// the order the execution took them, which depends on the schedule. Each thread holds the lock
/// across its reading, so that the order of the returned readings is the order they were taken in.
pub fn readings_from_threads(num_threads: usize, reads: usize) -> Vec<Instant> {
    let readings = Arc::new(Mutex::new(Vec::new()));
    let threads = (0..num_threads)
        .map(|_| {
            let readings = readings.clone();
            thread::spawn(move || {
                for _ in 0..reads {
                    let mut readings = readings.lock().unwrap();
                    readings.push(Instant::now());
                }
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    let readings = readings.lock().unwrap().clone();
    assert_eq!(readings.len(), num_threads * reads);
    readings
}
