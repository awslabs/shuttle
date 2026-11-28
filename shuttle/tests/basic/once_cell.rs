//! Tests for `shuttle::cell::OnceCell`.

use shuttle::cell::OnceCell;
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_dfs, thread};
use std::panic::{self, AssertUnwindSafe};
use test_log::test;

#[test]
fn set_and_get() {
    check_dfs(
        || {
            let cell = OnceCell::new();
            assert_eq!(cell.get(), None);
            assert_eq!(cell.set(1), Ok(()));
            assert_eq!(cell.set(2), Err(2));
            assert_eq!(cell.get(), Some(&1));
            assert_eq!(cell.into_inner(), Some(1));
        },
        None,
    );
}

#[test]
fn get_or_init_runs_once() {
    check_dfs(
        || {
            let cell = OnceCell::new();
            let mut runs = 0;
            assert_eq!(
                cell.get_or_init(|| {
                    runs += 1;
                    String::from("value")
                }),
                "value"
            );
            assert_eq!(cell.get_or_init(|| unreachable!()), "value");
            assert_eq!(runs, 1);
        },
        None,
    );
}

#[test]
#[should_panic(expected = "reentrant init")]
fn reentrant_init_panics() {
    check_dfs(
        || {
            let cell = OnceCell::new();
            cell.get_or_init(|| {
                cell.set(1).unwrap();
                2
            });
        },
        None,
    );
}

#[test]
fn a_panicking_init_leaves_the_cell_empty() {
    check_dfs(
        || {
            let cell = OnceCell::new();
            let result = panic::catch_unwind(AssertUnwindSafe(|| cell.get_or_init(|| panic!("no value"))));
            assert!(result.is_err());
            assert_eq!(cell.get(), None);
            assert_eq!(*cell.get_or_init(|| 3), 3);
        },
        None,
    );
}

#[test]
fn get_mut_and_take() {
    check_dfs(
        || {
            let mut cell = OnceCell::from(1);
            *cell.get_mut().unwrap() += 1;
            assert_eq!(cell.take(), Some(2));
            assert_eq!(cell.get(), None);
            assert_eq!(cell.get_mut(), None);
            assert_eq!(cell.take(), None);
        },
        None,
    );
}

#[test]
fn traits() {
    check_dfs(
        || {
            let empty = OnceCell::<u8>::default();
            assert_eq!(format!("{empty:?}"), "OnceCell(<uninit>)");
            let full = OnceCell::from(5);
            assert_eq!(format!("{full:?}"), "OnceCell(5)");
            assert_eq!(full.clone(), full);
            assert_eq!(empty.clone(), empty);
            assert_ne!(OnceCell::from(4), full);
        },
        None,
    );
}

#[test]
fn moved_into_a_thread() {
    check_dfs(
        || {
            let cell = OnceCell::new();
            let t = thread::spawn(move || *cell.get_or_init(|| 7));
            assert_eq!(t.join().unwrap(), 7);
        },
        None,
    );
}

/// A cell that tasks share.
struct Shared(OnceCell<usize>);

// SAFETY: the test orders every access with a mutex.
unsafe impl Sync for Shared {}

#[test]
fn shared_under_a_mutex() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(()));
            let cell = Arc::new(Shared(OnceCell::new()));
            let handles: Vec<_> = (0..2)
                .map(|i| {
                    let lock = Arc::clone(&lock);
                    let cell = Arc::clone(&cell);
                    thread::spawn(move || {
                        let _guard = lock.lock().unwrap();
                        *cell.0.get_or_init(|| i)
                    })
                })
                .collect();
            let values: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            assert_eq!(values[0], values[1]);
        },
        None,
    );
}
