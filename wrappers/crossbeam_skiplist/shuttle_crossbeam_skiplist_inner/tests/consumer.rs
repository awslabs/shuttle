//! The code in `shuttle-crossbeam-skiplist`'s `tests/consumer`, written against crossbeam-skiplist,
//! compiled unchanged against this crate, which is what `crossbeam_skiplist` is under the `shuttle`
//! feature, and run under Shuttle.

// The consumer code names this crate `crossbeam_skiplist`, as a dependent crate would.
extern crate shuttle_crossbeam_skiplist_inner as crossbeam_skiplist;

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
