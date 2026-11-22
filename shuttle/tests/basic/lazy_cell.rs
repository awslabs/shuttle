//! Tests for `shuttle::cell::LazyCell`.

use shuttle::cell::LazyCell;
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_dfs, thread};
use std::panic::{self, AssertUnwindSafe};
use test_log::test;

#[test]
fn force_runs_the_function_once() {
    check_dfs(
        || {
            let runs = std::cell::Cell::new(0);
            let lazy = LazyCell::new(|| {
                runs.set(runs.get() + 1);
                5
            });
            assert_eq!(LazyCell::get(&lazy), None);
            assert_eq!(*LazyCell::force(&lazy), 5);
            assert_eq!(*lazy, 5);
            assert_eq!(LazyCell::get(&lazy), Some(&5));
            assert_eq!(runs.get(), 1);
        },
        None,
    );
}

#[test]
fn force_mut_and_deref_mut() {
    check_dfs(
        || {
            let mut lazy = LazyCell::new(|| vec![1]);
            assert_eq!(LazyCell::get_mut(&mut lazy), None);
            LazyCell::force_mut(&mut lazy).push(2);
            lazy.push(3);
            assert_eq!(LazyCell::get_mut(&mut lazy).map(|v| v.len()), Some(3));
            assert_eq!(*lazy, [1, 2, 3]);
        },
        None,
    );
}

#[test]
#[should_panic(expected = "LazyCell instance has previously been poisoned")]
fn a_panicking_function_poisons_the_cell() {
    check_dfs(
        || {
            let lazy = LazyCell::new(|| -> u32 { panic!("no value") });
            let result = panic::catch_unwind(AssertUnwindSafe(|| *lazy));
            assert!(result.is_err());
            assert_eq!(LazyCell::get(&lazy), None);
            let _ = *lazy;
        },
        None,
    );
}

#[test]
fn default_debug_and_from() {
    check_dfs(
        || {
            let lazy = LazyCell::<Vec<u8>>::default();
            assert_eq!(format!("{lazy:?}"), "LazyCell(<uninit>)");
            assert!(lazy.is_empty());
            assert_eq!(format!("{lazy:?}"), "LazyCell([])");
            let ready: LazyCell<u32> = LazyCell::from(3);
            assert_eq!(LazyCell::get(&ready), Some(&3));
        },
        None,
    );
}

#[test]
fn moved_into_a_thread() {
    check_dfs(
        || {
            let lazy = LazyCell::new(|| 4);
            let t = thread::spawn(move || *lazy + 1);
            assert_eq!(t.join().unwrap(), 5);
        },
        None,
    );
}

/// A cell that tasks share.
struct Shared(LazyCell<usize>);

// SAFETY: the test orders every access with a mutex.
unsafe impl Sync for Shared {}

#[test]
fn shared_under_a_mutex() {
    fn make() -> usize {
        9
    }

    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(()));
            let lazy = Arc::new(Shared(LazyCell::new(make)));
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let lock = Arc::clone(&lock);
                    let lazy = Arc::clone(&lazy);
                    thread::spawn(move || {
                        let _guard = lock.lock().unwrap();
                        *lazy.0
                    })
                })
                .collect();
            for handle in handles {
                assert_eq!(handle.join().unwrap(), 9);
            }
        },
        None,
    );
}
