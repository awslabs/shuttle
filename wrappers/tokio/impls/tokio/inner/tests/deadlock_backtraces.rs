//! Checks what a deadlock report shows for a task waiting in a `JoinSet`.
//!
//! These tests need `SHUTTLE_CAPTURE_BACKTRACE`, which Shuttle reads once per process, so they live
//! in their own test binary and turn it on before they touch Shuttle.
use shuttle::{check_random, future};
use shuttle_tokio_impl_inner::sync::oneshot;
use shuttle_tokio_impl_inner::task::JoinSet;
use std::panic::{self, UnwindSafe};
use std::sync::Once;

/// Run `f`, which must deadlock, with backtrace capture on, and return the deadlock report.
fn deadlock_report(f: impl FnOnce() + UnwindSafe) -> String {
    static ENABLE: Once = Once::new();
    ENABLE.call_once(|| std::env::set_var(shuttle::CAPTURE_BACKTRACE, "1"));
    let payload = panic::catch_unwind(f).expect_err("test should deadlock");
    let report = *payload.downcast::<String>().expect("a deadlock panics with a String");
    assert!(report.starts_with("deadlock!"), "expected a deadlock, got: {report}");
    report
}

/// `JoinSet` waits for its tasks through a `FuturesUnordered`, which polls their join handles with
/// wakers of its own, so no waker clone records where `join_next` waits. The join handle records it
/// instead, and the report names the task it waits for.
#[test]
fn join_next_is_shown_waiting_for_its_task() {
    let report = deadlock_report(|| {
        check_random(
            || {
                future::block_on(async {
                    let (_tx, rx) = oneshot::channel::<()>();
                    let mut set = JoinSet::new();
                    set.spawn(async move {
                        let _ = rx.await;
                    });
                    while set.join_next().await.is_some() {}
                });
            },
            1,
        )
    });

    // The main thread, task 0, waits in `join_next` for the task it spawned, task 1.
    let main = report.split("\n, ").next().expect("a report lists at least one task");
    assert!(
        main.contains("(task main-thread(0)"),
        "the first entry is not the main thread:\n{main}"
    );
    assert!(
        main.contains("Waiting inside a combinator, 1 of 1, joining task main-thread(1):"),
        "not shown waiting for the task it joins:\n{main}"
    );
}
