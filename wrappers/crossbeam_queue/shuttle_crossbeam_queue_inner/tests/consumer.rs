//! The code in `shuttle-crossbeam-queue`'s `tests/consumer`, written against crossbeam-queue,
//! compiled unchanged against this crate, which is what `crossbeam_queue` is under the `shuttle`
//! feature, and run under Shuttle.

// The consumer code names this crate `crossbeam_queue`, as a dependent crate would.
extern crate shuttle_crossbeam_queue_inner as crossbeam_queue;

#[path = "../../tests/consumer/mod.rs"]
mod consumer;

mod thread {
    pub use shuttle::thread::*;
}

#[test]
fn consumer_code_runs_under_shuttle() {
    shuttle::check_random(consumer::exercise, 100);
}

#[test]
fn consumer_code_runs_under_pct() {
    shuttle::check_pct(consumer::exercise, 100, 3);
}

#[test]
fn consumer_code_replays() {
    shuttle::check_uncontrolled_nondeterminism(consumer::exercise, 100);
}
