//! The code in `shuttle-fastrand`'s `tests/consumer`, written against fastrand, compiled unchanged against this
//! crate, which is what `shuttle-fastrand` is under its `shuttle` feature, and run under Shuttle.

extern crate shuttle_fastrand_inner as fastrand;

#[path = "../../tests/consumer/mod.rs"]
mod consumer;

#[test]
fn consumer_code_runs_under_shuttle() {
    shuttle::check_random(consumer::exercise, 20);
}

#[test]
fn consumer_code_replays() {
    shuttle::check_uncontrolled_nondeterminism(consumer::exercise, 20);
}
