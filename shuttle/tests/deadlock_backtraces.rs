//! Checks which stack each task in a deadlock report is shown with.
//!
//! These tests need `SHUTTLE_CAPTURE_BACKTRACE`, which Shuttle reads once per process, so they live
//! in their own test binary and every test turns it on before it touches Shuttle.
//!
//! Release builds carry no line info, so the tests identify frames by function name. Each wait
//! lives in its own `#[inline(never)]` function, and never as a tail call, so that it keeps a
//! frame of its own.

use shuttle::sync::Barrier;
use shuttle::{check_dfs, future, thread};
use std::panic::{self, UnwindSafe};
use std::sync::Once;

/// Run `f`, which must deadlock, with backtrace capture on, and return the deadlock report.
fn deadlock_report(f: impl FnOnce() + UnwindSafe) -> String {
    static ENABLE: Once = Once::new();
    ENABLE.call_once(|| std::env::set_var(shuttle::CAPTURE_BACKTRACE, "1"));

    let payload = panic::catch_unwind(f).expect_err("test should deadlock");
    let report = *payload.downcast::<String>().expect("a deadlock panics with a String");
    assert!(report.starts_with("deadlock"), "expected a deadlock, got: {report}");
    report
}

/// Whether `entry` has a frame for the function `name` in this test binary.
fn has_frame(entry: &str, name: &str) -> bool {
    // A frame's function name ends its line, so a closure or a longer name that merely starts with
    // `name` does not match.
    entry.contains(&format!("deadlock_backtraces::{name}\n"))
}

#[inline(never)]
fn wait_in_destructor(barrier: &Barrier) {
    barrier.wait();
    std::hint::black_box(());
}

struct WaitsOnDrop(Barrier);

impl Drop for WaitsOnDrop {
    fn drop(&mut self) {
        wait_in_destructor(&self.0);
    }
}

/// Teardown drops the future of a task that is still parked when the execution ends. A destructor
/// that then blocks for good fails the test like a deadlock, and must be shown where it blocks.
#[test]
fn destructor_blocked_in_teardown_is_shown_where_it_blocks() {
    let report = deadlock_report(|| {
        check_dfs(
            || {
                future::spawn(async {
                    let _waits = WaitsOnDrop(Barrier::new(2));
                    std::future::pending::<()>().await;
                });
                // So that the future runs, and holds the value when the execution ends.
                thread::yield_now();
            },
            None,
        )
    });

    assert!(report.starts_with("deadlock while dropping"), "{report}");
    assert!(
        has_frame(&report, "wait_in_destructor"),
        "the destructor is not shown where it blocks:\n{report}"
    );
}
