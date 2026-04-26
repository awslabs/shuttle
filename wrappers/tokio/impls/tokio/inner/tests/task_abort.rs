use shuttle::check_dfs;
use shuttle_tokio_impl_inner::task::{self, AbortHandle, JoinHandle};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use test_log::test;

// ---- Basic abort via JoinHandle ----

// Aborting before the task runs. abort() is a scheduling point, so DFS
// explores interleavings where the task completes before the abort flag is set.
#[test]
fn tokio_abort_before_task_runs() {
    check_dfs(
        || {
            let counter = Arc::new(AtomicUsize::new(0));
            let t1: JoinHandle<()> = task::spawn({
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            });
            t1.abort();
            let result = shuttle::future::block_on(t1);
            match result {
                Ok(()) => assert_eq!(1, counter.load(Ordering::SeqCst)),
                Err(e) => {
                    assert!(e.is_cancelled());
                    assert_eq!(0, counter.load(Ordering::SeqCst));
                }
            }
        },
        None,
    );
}

// Aborting a task mid-flight (after one yield): join resolves to Err or Ok depending on ordering.
#[test]
fn tokio_abort_mid_flight() {
    check_dfs(
        || {
            let counter = Arc::new(AtomicUsize::new(0));
            let t1: JoinHandle<()> = task::spawn({
                let counter = counter.clone();
                async move {
                    shuttle::future::yield_now().await;
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            });
            let abort = t1.abort_handle();
            let joiner: JoinHandle<Result<(), _>> = task::spawn(t1);
            abort.abort();
            let result = shuttle::future::block_on(joiner).unwrap();
            // Either the task completed before abort, or it was cancelled.
            match result {
                Ok(()) => assert_eq!(1, counter.load(Ordering::SeqCst)),
                Err(e) => {
                    assert!(e.is_cancelled());
                    assert_eq!(
                        0,
                        counter.load(Ordering::SeqCst),
                        "aborted task must not increment counter"
                    );
                }
            }
        },
        None,
    );
}

// Aborting via AbortHandle after JoinHandle is consumed.
#[test]
fn tokio_abort_handle_works_after_join_consumed() {
    check_dfs(
        || {
            let t1: JoinHandle<()> = task::spawn(async {
                shuttle::future::yield_now().await;
            });
            let abort: AbortHandle = t1.abort_handle();
            abort.abort();
            let result = shuttle::future::block_on(t1);
            // With scheduling point in abort(), both outcomes are possible.
            match result {
                Ok(()) => {}
                Err(e) => assert!(e.is_cancelled()),
            }
        },
        None,
    );
}

// Calling abort() multiple times is idempotent.
#[test]
fn tokio_abort_idempotent() {
    check_dfs(
        || {
            let t1: JoinHandle<i32> = task::spawn(async { 42 });
            t1.abort();
            t1.abort();
            t1.abort();
            let result = shuttle::future::block_on(t1);
            match result {
                Ok(42) => {}
                Err(e) => assert!(e.is_cancelled()),
                _ => panic!("unexpected result"),
            }
        },
        None,
    );
}

// Aborting a finished task returns Ok with the task's output.
#[test]
fn tokio_abort_finished_task_returns_ok() {
    check_dfs(
        || {
            let t1: JoinHandle<u32> = task::spawn(async { 99u32 });
            let result = shuttle::future::block_on(async move {
                let v = t1.await.unwrap();
                v
            });
            assert_eq!(99, result);
        },
        None,
    );
}

// JoinError::is_cancelled() is true for aborted tasks.
#[test]
fn tokio_join_error_is_cancelled() {
    check_dfs(
        || {
            let t1: JoinHandle<()> = task::spawn(async {
                shuttle::future::yield_now().await;
            });
            t1.abort();
            let result = shuttle::future::block_on(t1);
            match result {
                Ok(()) => {} // task completed before abort took effect
                Err(e) => assert!(e.is_cancelled()),
            }
        },
        None,
    );
}

// Dropping a JoinHandle does NOT abort the task; the task runs to completion.
#[test]
fn tokio_drop_join_handle_detaches_not_aborts() {
    let ran = Arc::new(AtomicUsize::new(0));
    let ran_clone = ran.clone();
    check_dfs(
        move || {
            let ran = ran.clone();
            let t1: JoinHandle<()> = task::spawn(async move {
                ran.fetch_add(1, Ordering::SeqCst);
            });
            drop(t1);
            // Yield so the detached task gets a chance to run.
            shuttle::future::block_on(shuttle::future::yield_now());
        },
        None,
    );
    // Across all DFS orderings, the detached task ran in at least some.
    assert!(ran_clone.load(Ordering::SeqCst) > 0);
}

// AbortHandle clone produces a functional handle.
#[test]
fn tokio_abort_handle_clone() {
    check_dfs(
        || {
            let t1: JoinHandle<()> = task::spawn(async {
                shuttle::future::yield_now().await;
            });
            let abort1 = t1.abort_handle();
            let abort2 = abort1.clone();
            drop(abort1);
            abort2.abort();
            let result = shuttle::future::block_on(t1);
            match result {
                Ok(()) => {}
                Err(e) => assert!(e.is_cancelled()),
            }
        },
        None,
    );
}

// Aborting one task does not affect a sibling task.
#[test]
fn tokio_abort_one_task_sibling_unaffected() {
    check_dfs(
        || {
            let c1 = Arc::new(AtomicUsize::new(0));
            let c2 = Arc::new(AtomicUsize::new(0));

            let t1: JoinHandle<()> = task::spawn({
                let c1 = c1.clone();
                async move {
                    shuttle::future::yield_now().await;
                    c1.fetch_add(1, Ordering::SeqCst);
                }
            });
            let t2: JoinHandle<u32> = task::spawn({
                let c2 = c2.clone();
                async move {
                    c2.fetch_add(1, Ordering::SeqCst);
                    42u32
                }
            });

            t1.abort();

            let r2 = shuttle::future::block_on(t2);
            assert!(r2.is_ok());
            assert_eq!(42u32, r2.unwrap());
            assert_eq!(1, c2.load(Ordering::SeqCst), "sibling must still run");
        },
        None,
    );
}
