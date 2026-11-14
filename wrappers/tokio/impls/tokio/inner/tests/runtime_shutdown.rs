//! Shutting a runtime down stops its tasks, as in tokio (see the Shutdown section of tokio's
//! `Runtime` docs).

use futures::future::pending;
use shuttle::sync::mpsc;
use shuttle::{check_dfs, check_random, check_uncontrolled_nondeterminism};
use shuttle_tokio_impl_inner::runtime::{Builder, Handle, Runtime};
use shuttle_tokio_impl_inner::task::{self, JoinHandle};
use shuttle_tokio_impl_inner::time::{clear_triggers, trigger_timeouts};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use test_log::test;

fn runtime() -> Runtime {
    Builder::new_multi_thread().build().unwrap()
}

fn assert_cancelled<T: std::fmt::Debug>(task: JoinHandle<T>) {
    assert!(shuttle::future::block_on(task).unwrap_err().is_cancelled());
}

/// Sets a flag when dropped.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn drop_cancels_the_runtimes_tasks() {
    check_dfs(
        || {
            let rt = runtime();
            let task = rt.spawn(pending::<()>());
            drop(rt);
            assert_cancelled(task);
        },
        None,
    );
}

#[test]
#[allow(clippy::async_yields_async)] // The async blocks return a task's handle, to await it after the runtime is gone.
fn tasks_spawned_by_the_runtimes_tasks_and_in_its_block_on_are_its_own() {
    check_dfs(
        || {
            let rt = runtime();
            let from_block_on = rt.block_on(async { task::spawn(pending::<()>()) });
            let from_task = rt
                .block_on(rt.spawn(async { Handle::current().spawn(pending::<()>()) }))
                .unwrap();
            drop(rt);
            assert_cancelled(from_block_on);
            assert_cancelled(from_task);
        },
        None,
    );
}

#[test]
#[allow(clippy::async_yields_async)] // The async blocks return a task's handle, to await it after the runtime is gone.
fn dropping_a_runtime_leaves_other_runtimes_tasks_running() {
    check_dfs(
        || {
            let rt1 = runtime();
            let rt2 = runtime();
            let handle2 = rt2.handle().clone();
            // Spawned from rt1's context, onto rt2.
            let on_rt2 = rt1.block_on(async move {
                handle2.spawn(async {
                    task::yield_now().await;
                    42
                })
            });
            drop(rt1);
            assert_eq!(shuttle::future::block_on(on_rt2).unwrap(), 42);
        },
        None,
    );
}

#[test]
fn a_task_spawned_after_shutdown_is_cancelled_without_running() {
    check_dfs(
        || {
            let rt = runtime();
            let handle = rt.handle().clone();
            drop(rt);
            assert_cancelled(handle.spawn(async { panic!("polled after its runtime shut down") }));
            assert_cancelled(handle.spawn_blocking(|| panic!("ran after its runtime shut down")));
        },
        None,
    );
}

#[test]
fn drop_waits_until_cancelled_tasks_are_dropped() {
    check_dfs(
        || {
            let dropped = Arc::new(AtomicBool::new(false));
            let guard = SetOnDrop(dropped.clone());
            let rt = runtime();
            drop(rt.spawn(async move {
                let _guard = guard;
                pending::<()>().await
            }));
            drop(rt);
            assert!(dropped.load(Ordering::SeqCst));
        },
        None,
    );
}

#[test]
fn drop_waits_for_spawn_blocking_work_without_cancelling_it() {
    check_dfs(
        || {
            let done = Arc::new(AtomicBool::new(false));
            let flag = done.clone();
            let rt = runtime();
            drop(rt.spawn_blocking(move || flag.store(true, Ordering::SeqCst)));
            drop(rt);
            assert!(done.load(Ordering::SeqCst));
        },
        None,
    );
}

#[test]
fn shutdown_background_doesnt_wait() {
    check_dfs(
        || {
            let rt = runtime();
            let (tx, rx) = mpsc::channel::<()>();
            let blocked = rt.spawn_blocking(move || rx.recv().unwrap());
            // Dropping the runtime here would wait for the blocking task forever.
            rt.shutdown_background();
            tx.send(()).unwrap();
            shuttle::future::block_on(blocked).unwrap();
        },
        None,
    );
}

#[test]
fn shutdown_timeout_waits_for_tasks_to_stop() {
    check_dfs(
        || {
            clear_triggers();
            let done = Arc::new(AtomicBool::new(false));
            let flag = done.clone();
            let rt = runtime();
            drop(rt.spawn_blocking(move || flag.store(true, Ordering::SeqCst)));
            rt.shutdown_timeout(Duration::from_secs(1));
            assert!(done.load(Ordering::SeqCst));
        },
        None,
    );
}

#[test]
fn shutdown_timeout_stops_waiting_once_triggered() {
    check_dfs(
        || {
            clear_triggers();
            let rt = runtime();
            let (tx, rx) = mpsc::channel::<()>();
            let blocked = rt.spawn_blocking(move || rx.recv().unwrap());
            // Without the trigger, shutdown_timeout would wait for the blocking task forever.
            trigger_timeouts(|_| true);
            rt.shutdown_timeout(Duration::from_secs(1));
            tx.send(()).unwrap();
            shuttle::future::block_on(blocked).unwrap();
        },
        None,
    );
}

#[test]
#[should_panic(expected = "Cannot drop a runtime in a context where blocking is not allowed")]
fn dropping_a_runtime_in_block_on_panics() {
    check_random(
        || {
            let outer = runtime();
            outer.block_on(async { drop(runtime()) });
        },
        1,
    );
}

#[test]
#[should_panic(expected = "Cannot drop a runtime in a context where blocking is not allowed")]
fn dropping_a_runtime_in_a_task_panics() {
    check_random(
        || {
            let outer = runtime();
            let inner = runtime();
            outer.block_on(outer.spawn(async move { drop(inner) })).unwrap();
        },
        1,
    );
}

#[test]
fn shutting_down_without_waiting_in_an_asynchronous_context_is_allowed() {
    check_dfs(
        || {
            let outer = runtime();
            outer.block_on(async {
                runtime().shutdown_background();
                runtime().shutdown_timeout(Duration::ZERO);
            });
            // So is dropping one in `spawn_blocking` work, where blocking is allowed.
            let inner = runtime();
            outer.block_on(outer.spawn_blocking(move || drop(inner))).unwrap();
        },
        None,
    );
}

#[test]
fn threads_and_shuttle_tasks_spawned_in_a_runtimes_context_are_outside_it() {
    check_dfs(
        || {
            let rt = runtime();
            // Both inherit the runtime context's label, but aren't in the context: as in tokio, a
            // runtime may be dropped on them.
            rt.block_on(async {
                shuttle::thread::spawn(|| drop(runtime())).join().unwrap();
                shuttle::future::spawn(async { drop(runtime()) }).await.unwrap();
            });
        },
        None,
    );
}

#[test]
fn a_runtime_dropped_outside_an_execution_or_by_teardown_does_nothing() {
    drop(runtime());
    check_dfs(
        || {
            let rt = runtime();
            drop(rt.spawn(pending::<()>()));
            // A detached task that owns the runtime, still parked when the execution ends: teardown
            // drops the task, and with it the runtime, in an asynchronous context.
            drop(shuttle::future::spawn(async move {
                let _rt = rt;
                pending::<()>().await
            }));
        },
        None,
    );
}

#[test]
fn shutdown_has_no_uncontrolled_nondeterminism() {
    check_uncontrolled_nondeterminism(
        || {
            let rt = runtime();
            for _ in 0..3 {
                drop(rt.spawn(async { task::yield_now().await }));
            }
            drop(rt.spawn_blocking(|| ()));
            drop(rt);
        },
        100,
    );
}
