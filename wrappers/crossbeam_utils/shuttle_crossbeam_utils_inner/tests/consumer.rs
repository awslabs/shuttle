//! The code in `shuttle-crossbeam-utils`'s `tests/consumer`, written against crossbeam-utils,
//! compiled unchanged against this crate, which is what the wrapper is under the `shuttle` feature,
//! and run under Shuttle.

pub use shuttle::{sync, thread};
pub use shuttle_crossbeam_utils_inner as crossbeam_utils;
pub use shuttle_time_inner::time;

#[path = "../../tests/consumer/mod.rs"]
mod consumer;

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
