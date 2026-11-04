use shuttle::sync::atomic::AtomicBool;
use shuttle::{check_dfs, future, thread};
use shuttle_tokio_impl_inner::sync::OnceCell;
use std::sync::atomic::Ordering;
use std::sync::Arc;
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
