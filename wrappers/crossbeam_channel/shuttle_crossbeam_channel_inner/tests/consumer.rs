//! The code in `shuttle-crossbeam-channel`'s `tests/consumer`, written against crossbeam-channel,
//! compiled unchanged against this crate, which is what the wrapper is under the `shuttle` feature,
//! and run under Shuttle.

pub use shuttle::thread;
pub use shuttle_crossbeam_channel_inner as crossbeam_channel;
pub use shuttle_time_inner::time;

#[path = "../../tests/consumer/mod.rs"]
mod consumer;

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
