// Each test binary that includes this module uses some of it.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

/// What `body` returned in the executions of an exhaustive search of its schedules. The search
/// allows random data, which a selection with more than one ready operation draws, though it does
/// not explore its values.
pub fn dfs_outcomes<T: Ord + Send + 'static>(body: impl Fn() -> T + Send + Sync + 'static) -> BTreeSet<T> {
    // A `std::sync::Mutex` on purpose: it holds what the executions produced, and so has to outlive
    // them.
    let outcomes = Arc::new(Mutex::new(BTreeSet::new()));
    let sink = Arc::clone(&outcomes);
    let runner = shuttle::Runner::new(
        shuttle::scheduler::DfsScheduler::new(None, true),
        shuttle::Config::new(),
    );
    runner.run(move || {
        sink.lock().unwrap().insert(body());
    });
    std::mem::take(&mut *outcomes.lock().unwrap())
}

/// What `body` returned in `iterations` random executions.
pub fn random_outcomes<T: Ord + Send + 'static>(
    body: impl Fn() -> T + Send + Sync + 'static,
    iterations: usize,
) -> BTreeSet<T> {
    let outcomes = Arc::new(Mutex::new(BTreeSet::new()));
    let sink = Arc::clone(&outcomes);
    shuttle::check_random(
        move || {
            sink.lock().unwrap().insert(body());
        },
        iterations,
    );
    std::mem::take(&mut *outcomes.lock().unwrap())
}

/// What `body` returned in each of `executions` executions of a random scheduler seeded with
/// `seed`, in order.
pub fn per_execution<T: Send + 'static>(body: fn() -> T, seed: u64, executions: usize) -> Vec<T> {
    let collected = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&collected);
    shuttle::check_random_with_seed(move || sink.lock().unwrap().push(body()), seed, executions);
    let collected = std::mem::take(&mut *collected.lock().unwrap());
    assert_eq!(collected.len(), executions, "every execution ran the body once");
    collected
}

/// A value that counts its drops, to see when a channel drops its messages.
#[derive(Debug)]
pub struct Counted(pub Arc<std::sync::atomic::AtomicUsize>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
