//! Tests for `shuttle::cell::RefCell` and its `Ref` and `RefMut`.

use shuttle::cell::{Ref, RefCell, RefMut};
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_dfs, thread};
use std::fmt::Debug;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use test_log::test;

#[test]
fn borrow_and_borrow_mut() {
    check_dfs(
        || {
            let cell = RefCell::new(vec![1]);
            cell.borrow_mut().push(2);
            let a = cell.borrow();
            let b = cell.borrow();
            assert_eq!(*a, [1, 2]);
            assert_eq!(b.len(), 2);
            assert!(cell.try_borrow_mut().is_err());
            drop((a, b));
            let mut m = cell.borrow_mut();
            assert!(cell.try_borrow().is_err());
            assert!(cell.try_borrow_mut().is_err());
            m.push(3);
            drop(m);
            assert_eq!(cell.into_inner(), [1, 2, 3]);
        },
        None,
    );
}

#[test]
fn errors_are_std_errors() {
    check_dfs(
        || {
            let cell = RefCell::new(0);
            let m = cell.borrow_mut();
            let error: std::cell::BorrowError = cell.try_borrow().map(|_| ()).unwrap_err();
            assert_eq!(error.to_string(), "RefCell already mutably borrowed");
            drop(m);
            let r = cell.borrow();
            let error: std::cell::BorrowMutError = cell.try_borrow_mut().map(|_| ()).unwrap_err();
            assert_eq!(error.to_string(), "RefCell already borrowed");
            drop(r);
        },
        None,
    );
}

#[test]
#[should_panic(expected = "RefCell already mutably borrowed")]
fn borrow_panics_while_mutably_borrowed() {
    check_dfs(
        || {
            let cell = RefCell::new(0);
            let _m = cell.borrow_mut();
            let _r = cell.borrow();
        },
        None,
    );
}

#[test]
#[should_panic(expected = "RefCell already borrowed")]
fn borrow_mut_panics_while_borrowed() {
    check_dfs(
        || {
            let cell = RefCell::new(0);
            let _r = cell.borrow();
            let _m = cell.borrow_mut();
        },
        None,
    );
}

#[test]
fn replace_swap_take() {
    check_dfs(
        || {
            let a = RefCell::new(String::from("a"));
            let b = RefCell::new(String::from("b"));
            assert_eq!(a.replace(String::from("c")), "a");
            assert_eq!(a.replace_with(|s| format!("{s}d")), "c");
            a.swap(&b);
            assert_eq!(a.take(), "b");
            assert_eq!(*a.borrow(), "");
            assert_eq!(b.into_inner(), "cd");
        },
        None,
    );
}

#[test]
fn get_mut_as_ptr_and_unguarded() {
    check_dfs(
        || {
            let mut cell = RefCell::new(1);
            *cell.get_mut() += 1;
            unsafe { *cell.as_ptr() += 1 };
            assert_eq!(unsafe { cell.try_borrow_unguarded() }.copied().unwrap(), 3);
            let _m = cell.borrow_mut();
            assert!(unsafe { cell.try_borrow_unguarded() }.is_err());
        },
        None,
    );
}

#[test]
fn ref_clone_map_and_split() {
    check_dfs(
        || {
            let cell = RefCell::new((1, String::from("two")));
            let r = cell.borrow();
            let copy = Ref::clone(&r);
            drop(r);
            let first = Ref::map(copy, |pair| &pair.0);
            assert_eq!(*first, 1);
            let second = Ref::filter_map(cell.borrow(), |pair| Some(&pair.1)).ok().unwrap();
            assert!(Ref::filter_map(cell.borrow(), |_| None::<&u8>).is_err());
            let (a, b) = Ref::map_split(cell.borrow(), |pair| (&pair.0, &pair.1));
            assert_eq!((*a, b.as_str(), second.as_str()), (1, "two", "two"));
            assert!(cell.try_borrow_mut().is_err());
            drop((first, second, a, b));
            assert!(cell.try_borrow_mut().is_ok());
        },
        None,
    );
}

#[test]
fn ref_mut_map_and_split() {
    check_dfs(
        || {
            let cell = RefCell::new([1, 2, 3, 4]);
            {
                let mut slice = RefMut::map(cell.borrow_mut(), |array| &mut array[..]);
                slice[0] = 10;
            }
            {
                let mut last = RefMut::filter_map(cell.borrow_mut(), |array| array.last_mut())
                    .ok()
                    .unwrap();
                *last = 40;
            }
            assert!(RefMut::filter_map(cell.borrow_mut(), |_| None::<&mut u8>).is_err());
            let (mut left, mut right) = RefMut::map_split(cell.borrow_mut(), |array| array.split_at_mut(2));
            left[1] = 20;
            right[0] = 30;
            drop(left);
            assert!(cell.try_borrow().is_err());
            drop(right);
            assert_eq!(*cell.borrow(), [10, 20, 30, 40]);
        },
        None,
    );
}

#[test]
fn traits() {
    check_dfs(
        || {
            let a = RefCell::new(1);
            let mut b = a.clone();
            *b.get_mut() = 2;
            assert!(a < b);
            assert_eq!(a.cmp(&b), std::cmp::Ordering::Less);
            b.clone_from(&a);
            assert_eq!(a, b);
            assert_eq!(RefCell::from(5), RefCell::new(5));
            assert_eq!(RefCell::<u8>::default().into_inner(), 0);
            assert_eq!(format!("{a:?}"), "RefCell { value: 1 }");
            let m = a.borrow_mut();
            assert_eq!(format!("{a:?}"), "RefCell { value: <borrowed> }");
            assert_eq!(format!("{m} {m:?}"), "1 1");
        },
        None,
    );
}

#[test]
fn unsized_values() {
    check_dfs(
        || {
            let array = RefCell::new([1, 2, 3]);
            let slice: &RefCell<[i32]> = &array;
            slice.borrow_mut()[0] = 4;
            assert_eq!(slice.borrow().len(), 3);
            let shared: Rc<RefCell<dyn Debug>> = Rc::new(RefCell::new(5));
            assert_eq!(format!("{:?}", shared.borrow()), "5");
            assert_eq!(*array.borrow(), [4, 2, 3]);
        },
        None,
    );
}

#[test]
fn a_panic_ends_the_borrow() {
    check_dfs(
        || {
            let cell = RefCell::new(0);
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                let mut m = cell.borrow_mut();
                *m = 1;
                panic!("while borrowed");
            }));
            assert!(result.is_err());
            assert_eq!(*cell.borrow(), 1);
        },
        None,
    );
}

#[test]
fn moved_into_a_thread() {
    check_dfs(
        || {
            let cell = RefCell::new(vec![1]);
            let t = thread::spawn(move || {
                cell.borrow_mut().push(2);
                cell.into_inner()
            });
            assert_eq!(t.join().unwrap(), [1, 2]);
        },
        None,
    );
}

/// A `RefCell` that tasks share.
struct Shared(RefCell<Vec<u32>>);

// SAFETY: the test orders every access with a mutex.
unsafe impl Sync for Shared {}

#[test]
fn shared_under_a_mutex() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(()));
            let list = Arc::new(Shared(RefCell::new(Vec::new())));
            let handles: Vec<_> = (0..2)
                .map(|i| {
                    let lock = Arc::clone(&lock);
                    let list = Arc::clone(&list);
                    thread::spawn(move || {
                        let _guard = lock.lock().unwrap();
                        list.0.borrow_mut().push(i);
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
            let _guard = lock.lock().unwrap();
            assert_eq!(list.0.borrow().len(), 2);
        },
        None,
    );
}
