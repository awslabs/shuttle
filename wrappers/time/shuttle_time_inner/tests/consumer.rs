//! The code in `shuttle-time`'s `tests/consumer`, written against `std::time`, compiled unchanged
//! against this crate, which is what `shuttle_time::time` is under the `shuttle` feature, and run
//! under Shuttle.

pub use shuttle_time_inner::time;

#[path = "../../tests/consumer/mod.rs"]
mod consumer;

/// Stands in for a crate built on `std::time` itself.
mod real_crate {
    pub struct Record {
        pub at: std::time::SystemTime,
    }

    pub fn echo(at: std::time::SystemTime) -> std::time::SystemTime {
        at
    }
}

#[test]
fn consumer_code_runs_under_shuttle() {
    shuttle::check_random(consumer::exercise, 50);
}

#[test]
fn consumer_code_replays() {
    shuttle::check_uncontrolled_nondeterminism(consumer::exercise, 50);
}

#[test]
fn consumer_code_runs_under_dfs() {
    shuttle::check_dfs(consumer::exercise, None);
}
