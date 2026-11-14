//! The code in `shuttle-io-uring`'s `tests/consumer`, written against io-uring,
//! compiled unchanged against this crate, which is what `shuttle-io-uring` is
//! under its `shuttle` feature, and run under Shuttle.

#![cfg(target_os = "linux")]

extern crate shuttle_io_uring_impl as io_uring;

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
    shuttle::check_uncontrolled_nondeterminism(consumer::exercise, 50);
}
