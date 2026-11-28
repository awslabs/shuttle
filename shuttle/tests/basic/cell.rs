//! Tests for `shuttle::cell::Cell`.

use shuttle::cell::Cell;
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_dfs, thread};
use std::rc::Rc;
use test_log::test;

#[test]
fn get_set_replace_update_take() {
    check_dfs(
        || {
            let cell = Cell::new(1);
            assert_eq!(cell.get(), 1);
            cell.set(2);
            assert_eq!(cell.replace(3), 2);
            cell.update(|x| x * 10);
            assert_eq!(cell.get(), 30);
            assert_eq!(cell.take(), 30);
            assert_eq!(cell.get(), 0);
            assert_eq!(cell.into_inner(), 0);
        },
        None,
    );
}

#[test]
fn values_that_arent_copy() {
    check_dfs(
        || {
            let a = Cell::new(String::from("a"));
            let b = Cell::new(String::from("b"));
            a.swap(&b);
            a.swap(&a);
            assert_eq!(b.replace(String::from("c")), "a");
            assert_eq!(a.take(), "b");
            assert_eq!(a.into_inner(), "");
            assert_eq!(b.into_inner(), "c");
        },
        None,
    );
}

#[test]
fn set_drops_the_old_value() {
    check_dfs(
        || {
            let first = Rc::new(());
            let cell = Cell::new(Rc::clone(&first));
            cell.set(Rc::new(()));
            assert_eq!(Rc::strong_count(&first), 1);
            let second = cell.take();
            assert_eq!(Rc::strong_count(&second), 1);
        },
        None,
    );
}

#[test]
fn get_mut_and_as_ptr() {
    check_dfs(
        || {
            let mut cell = Cell::new(1);
            *cell.get_mut() += 1;
            unsafe { *cell.as_ptr() += 1 };
            assert_eq!(cell.get(), 3);
        },
        None,
    );
}

#[test]
fn traits() {
    check_dfs(
        || {
            let a = Cell::new(1);
            let b = a.clone();
            b.set(2);
            assert_eq!(a.get(), 1);
            assert!(a < b);
            assert_eq!(a.cmp(&b), std::cmp::Ordering::Less);
            assert_eq!(a, Cell::from(1));
            assert_eq!(Cell::<u8>::default().get(), 0);
            assert_eq!(format!("{a:?}"), "Cell { value: 1 }");
        },
        None,
    );
}

#[test]
fn moved_into_a_thread() {
    check_dfs(
        || {
            let cell = Cell::new(1);
            let t = thread::spawn(move || {
                cell.set(cell.get() + 1);
                cell.get()
            });
            assert_eq!(t.join().unwrap(), 2);
        },
        None,
    );
}

/// A cell that tasks share. Each test orders the accesses that this allows.
struct Shared(Cell<u64>);

// SAFETY: the test orders every access with a mutex.
unsafe impl Sync for Shared {}

#[test]
fn shared_under_a_mutex() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(()));
            let counter = Arc::new(Shared(Cell::new(0)));
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let lock = Arc::clone(&lock);
                    let counter = Arc::clone(&counter);
                    thread::spawn(move || {
                        let _guard = lock.lock().unwrap();
                        counter.0.update(|x| x + 1);
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
            let _guard = lock.lock().unwrap();
            assert_eq!(counter.0.get(), 2);
        },
        None,
    );
}
