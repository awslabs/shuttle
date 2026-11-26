//! The code in `shuttle-spin`'s `tests/consumer`, written against spin, compiled unchanged against
//! this crate, which is what `shuttle-spin` is under its `shuttle` feature, and run under Shuttle.

extern crate shuttle_spin_inner as spin;

#[path = "../../tests/consumer/mod.rs"]
mod consumer;

#[test]
fn consumer_code_runs_under_shuttle() {
    shuttle::check_random(consumer::exercise, 20);
}

#[test]
fn consumer_code_runs_under_dfs() {
    // One task, so one schedule; every scheduling point has a single choice.
    shuttle::check_dfs(consumer::exercise, None);
}

#[test]
fn consumer_code_replays() {
    shuttle::check_uncontrolled_nondeterminism(consumer::exercise, 20);
}
