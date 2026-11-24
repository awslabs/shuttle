use shuttle::sync::atomic::AtomicBool;
use shuttle::{check_dfs, future, thread};
use shuttle_tokio_impl_inner::sync::OnceCell;
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::task::Context;
use test_log::test;

/// Regression test: Shuttle must be able to schedule another task between the last operation of
/// the initializer of `get_or_init` and the store of the value.
///
/// The test program wrongly assumes that once it sees `ready`, which the initializer stores last,
/// the cell has a value. That fails when the other task runs between the initializer and the
/// store, which Shuttle only explores if the store takes a scheduling point first.
#[test]
#[should_panic(expected = "value not stored yet")]
fn get_or_init_store_after_initializer_is_a_separate_step() {
    check_dfs(
        || {
            let cell = Arc::new(OnceCell::new());
            let ready = Arc::new(AtomicBool::new(false));
            let reader = {
                let (cell, ready) = (cell.clone(), ready.clone());
                thread::spawn(move || {
                    if ready.load(Ordering::SeqCst) {
                        // BUG (in this test program): assumes the value has already been stored.
                        assert!(cell.get().is_some(), "value not stored yet");
                    }
                })
            };
            future::block_on(cell.get_or_init(|| async {
                ready.store(true, Ordering::SeqCst);
                1
            }));
            reader.join().unwrap();
        },
        None,
    );
}

/// Regression test: Shuttle must be able to schedule another task between the last operation of
/// the initializer of `get_or_try_init` and the store of the value.
///
/// The test program wrongly assumes that once it sees `ready`, which the initializer stores last,
/// the cell has a value. That fails when the other task runs between the initializer and the
/// store, which Shuttle only explores if the store takes a scheduling point first.
#[test]
#[should_panic(expected = "value not stored yet")]
fn get_or_try_init_store_after_initializer_is_a_separate_step() {
    check_dfs(
        || {
            let cell = Arc::new(OnceCell::new());
            let ready = Arc::new(AtomicBool::new(false));
            let reader = {
                let (cell, ready) = (cell.clone(), ready.clone());
                thread::spawn(move || {
                    if ready.load(Ordering::SeqCst) {
                        // BUG (in this test program): assumes the value has already been stored.
                        assert!(cell.get().is_some(), "value not stored yet");
                    }
                })
            };
            future::block_on(cell.get_or_try_init(|| async {
                ready.store(true, Ordering::SeqCst);
                Ok::<_, ()>(1)
            }))
            .unwrap();
            reader.join().unwrap();
        },
        None,
    );
}

/// Leaves `cell` empty, with its semaphore closed. In one execution, an unfinished `get_or_init`
/// takes the semaphore's permit. After the execution, the `get_or_init` is dropped, and with it the
/// permit. Shuttle's `BatchSemaphore` closes itself when a permit is released outside an execution.
fn close_while_empty(cell: &'static OnceCell<u32>) {
    type Init = Pin<Box<dyn Future<Output = ()> + Send>>;
    let unfinished: Arc<Mutex<Option<Init>>> = Arc::default();
    let slot = unfinished.clone();
    check_dfs(
        move || {
            let mut init: Init = Box::pin(async move {
                cell.get_or_init(std::future::pending).await;
            });
            // The poll takes the permit, and then waits forever in the initializer.
            let mut cx = Context::from_waker(futures::task::noop_waker_ref());
            assert!(init.as_mut().poll(&mut cx).is_pending());
            *slot.lock().unwrap() = Some(init);
        },
        None,
    );
    // Outside an execution, the release of the permit closes the semaphore. The release's
    // scheduling point also panics there.
    let init = unfinished.lock().unwrap().take();
    let _ = catch_unwind(AssertUnwindSafe(move || drop(init)));
    assert!(!cell.initialized());
}

/// Regression test: `get_or_init` must not return a reference into an empty cell when it finds the
/// semaphore closed.
#[test]
#[should_panic(expected = "its semaphore is closed")]
fn get_or_init_after_the_semaphore_closed_while_the_cell_was_empty() {
    static CELL: OnceCell<u32> = OnceCell::const_new();
    close_while_empty(&CELL);
    check_dfs(
        || {
            future::block_on(CELL.get_or_init(|| async { 1 }));
            assert!(CELL.initialized(), "get_or_init returned, but the cell is empty");
        },
        None,
    );
}

/// Regression test: `get_or_try_init` must not return a reference into an empty cell when it finds
/// the semaphore closed.
#[test]
#[should_panic(expected = "its semaphore is closed")]
fn get_or_try_init_after_the_semaphore_closed_while_the_cell_was_empty() {
    static CELL: OnceCell<u32> = OnceCell::const_new();
    close_while_empty(&CELL);
    check_dfs(
        || {
            future::block_on(CELL.get_or_try_init(|| async { Ok::<_, ()>(1) })).unwrap();
            assert!(CELL.initialized(), "get_or_try_init returned, but the cell is empty");
        },
        None,
    );
}
