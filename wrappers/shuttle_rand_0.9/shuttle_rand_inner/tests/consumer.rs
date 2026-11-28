//! The code in `shuttle-rand_0_9`'s `tests/consumer`, written against rand 0.9, compiled unchanged against this
//! crate, which is what `shuttle-rand_0_9` is under its `shuttle` feature, and run under Shuttle.

extern crate shuttle_rand_0_9_inner as rand;

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
