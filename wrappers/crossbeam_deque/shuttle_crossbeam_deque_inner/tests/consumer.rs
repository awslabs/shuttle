//! The code in `shuttle-crossbeam-deque`'s `tests/consumer`, written against crossbeam-deque,
//! compiled unchanged against this crate, which is what `crossbeam_deque` is under the `shuttle`
//! feature, and run under Shuttle.

// The consumer code names this crate `crossbeam_deque`, as a dependent crate would.
extern crate shuttle_crossbeam_deque_inner as crossbeam_deque;

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
