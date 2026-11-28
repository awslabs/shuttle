//! The code in `shuttle-once_cell`'s `tests/consumer`, written against once_cell, compiled unchanged
//! against this crate, which is what `shuttle-once_cell` is under its `shuttle` feature, and run
//! under Shuttle.

extern crate shuttle_once_cell_inner as once_cell;

#[path = "../../tests/consumer/mod.rs"]
mod consumer;

use shuttle::scheduler::RandomScheduler;
use shuttle::{Runner, thread};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

fn exercise() -> consumer::Seen {
    consumer::exercise();
    let workers = (0..3)
        .map(|i| thread::spawn(move || consumer::worker(i)))
        .collect::<Vec<_>>();
    let seen = workers.into_iter().map(|w| w.join().unwrap()).collect::<Vec<_>>();
    consumer::check(&seen);
    seen[0].clone()
}

#[test]
fn consumer_code_runs_under_shuttle() {
    shuttle::check_random(
        || {
            exercise();
        },
        50,
    );
}

#[test]
fn consumer_code_replays() {
    shuttle::check_uncontrolled_nondeterminism(
        || {
            exercise();
        },
        50,
    );
}

// The statics start empty in every execution, so each worker wins the race for them in some
// execution, which a test of once_cell itself would never see after the first.
#[test]
fn every_worker_wins_in_some_execution() {
    let winners = Arc::new(Mutex::new(HashSet::new()));
    let winners_clone = Arc::clone(&winners);
    let runner = Runner::new(RandomScheduler::new_from_seed(0x0CE11, 300), Default::default());
    runner.run(move || {
        winners_clone.lock().unwrap().insert(exercise().winner);
    });
    assert_eq!(*winners.lock().unwrap(), HashSet::from([0, 1, 2]));
}
