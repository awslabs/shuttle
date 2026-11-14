//! Scoped threads under Shuttle: the threads are tasks, the scope waits for them, and the borrows
//! they hold stay valid.

mod common;

use common::dfs_outcomes;
use shuttle::sync::atomic::{AtomicUsize, Ordering};
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_dfs, check_random, thread};
use shuttle_crossbeam_utils_inner::thread as cb_thread;

#[test]
fn a_scope_waits_for_a_thread_nobody_joins() {
    check_dfs(
        || {
            let done = AtomicUsize::new(0);
            cb_thread::scope(|s| {
                for _ in 0..2 {
                    s.spawn(|_| {
                        thread::yield_now();
                        done.fetch_add(1, Ordering::SeqCst);
                    });
                }
            })
            .unwrap();
            // The scope has ended, so both threads have finished, in every schedule.
            assert_eq!(done.load(Ordering::SeqCst), 2);
        },
        None,
    );
}

#[test]
fn a_scoped_thread_borrows_the_environment() {
    check_dfs(
        || {
            let values = [1u32, 2, 3];
            let total = cb_thread::scope(|s| {
                let handles = values.iter().map(|value| s.spawn(move |_| *value)).collect::<Vec<_>>();
                handles.into_iter().map(|h| h.join().unwrap()).sum::<u32>()
            })
            .unwrap();
            assert_eq!(total, 6);
        },
        None,
    );
}

#[test]
fn a_scoped_thread_spawns_into_the_same_scope() {
    check_dfs(
        || {
            let depth = Arc::new(AtomicUsize::new(0));
            cb_thread::scope(|s| {
                let outer = Arc::clone(&depth);
                s.spawn(move |s| {
                    outer.fetch_add(1, Ordering::SeqCst);
                    let inner = Arc::clone(&outer);
                    // A child spawned by a child, into the scope the parent was given.
                    s.spawn(move |_| inner.fetch_add(1, Ordering::SeqCst));
                });
            })
            .unwrap();
            assert_eq!(depth.load(Ordering::SeqCst), 2);
        },
        None,
    );
}

// The scheduler decides in which order the scope's threads run, so both orders happen.
#[test]
fn the_scheduler_orders_the_scopes_threads() {
    let orders = dfs_outcomes(|| {
        let order = Mutex::new(Vec::new());
        let shared = &order;
        cb_thread::scope(|s| {
            for id in 0..2u8 {
                s.spawn(move |_| shared.lock().unwrap().push(id));
            }
        })
        .unwrap();
        order.into_inner().unwrap()
    });
    assert_eq!(orders.len(), 2, "{orders:?}");
}

#[test]
fn a_named_scoped_thread_through_the_builder() {
    check_dfs(
        || {
            cb_thread::scope(|s| {
                let handle = s
                    .builder()
                    .name("worker".to_string())
                    .stack_size(1 << 16)
                    .spawn(|_| thread::current().name().map(str::to_string))
                    .unwrap();
                assert_eq!(handle.thread().name(), Some("worker"));
                assert_eq!(handle.join().unwrap().as_deref(), Some("worker"));
            })
            .unwrap();
        },
        None,
    );
}

// A scoped thread is a Shuttle task, so it can use Shuttle's primitives and block on them. A real
// thread cannot: Shuttle's primitives panic on a thread it does not know.
#[test]
fn a_scoped_thread_is_a_shuttle_task() {
    check_dfs(
        || {
            let lock = Mutex::new(0u32);
            cb_thread::scope(|s| {
                for _ in 0..2 {
                    s.spawn(|_| *lock.lock().unwrap() += 1);
                }
            })
            .unwrap();
            assert_eq!(lock.into_inner().unwrap(), 2);
        },
        None,
    );
}

/// The real crate spawns an OS thread, which Shuttle does not know, so Shuttle's `Mutex` panics
/// there with "Are you accessing a Shuttle primitive outside of a Shuttle test?", which crossbeam's
/// `scope` returns as an `Err`. That panic's message goes to stderr while this test runs.
#[test]
fn the_real_crates_scope_is_not_a_shuttle_task() {
    check_random(
        || {
            let lock = Mutex::new(0u32);
            let result = crossbeam_utils::thread::scope(|s| {
                s.spawn(|_| *lock.lock().unwrap() += 1);
            });
            assert!(result.is_err(), "a real thread used a Shuttle primitive");
            assert_eq!(lock.into_inner().unwrap(), 0);
        },
        1,
    );
}

#[test]
fn a_scope_returns_what_its_body_returned() {
    check_random(
        || {
            let value = cb_thread::scope(|_| 42u8).unwrap();
            assert_eq!(value, 42);
            let _ = format!(
                "{:?}",
                cb_thread::scope(|s| {
                    let handle = s.spawn(|_| ());
                    let text = format!("{:?} {:?}", s, handle);
                    handle.join().unwrap();
                    text
                })
                .unwrap()
            );
        },
        5,
    );
}
