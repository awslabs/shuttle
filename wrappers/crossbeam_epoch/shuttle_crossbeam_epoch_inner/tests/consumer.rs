//! The code in `shuttle-crossbeam-epoch`'s `tests/consumer`, written against crossbeam-epoch,
//! compiled unchanged against this crate, which is what `crossbeam_epoch` is under the `shuttle`
//! feature, and run under Shuttle.

// The consumer code names this crate `crossbeam_epoch`, as a dependent crate would.
extern crate shuttle_crossbeam_epoch_inner as crossbeam_epoch;

#[path = "../../tests/consumer/mod.rs"]
mod consumer;

mod thread {
    pub use shuttle::thread::*;
}

#[test]
fn consumer_code_runs_under_shuttle() {
    shuttle::check_random(consumer::exercise, 50);
}

#[test]
fn consumer_code_runs_under_pct() {
    shuttle::check_pct(consumer::exercise, 50, 3);
}

#[test]
fn consumer_code_replays() {
    shuttle::check_uncontrolled_nondeterminism(consumer::exercise, 50);
}
