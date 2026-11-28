//! Tests for the API of `shuttle::cell::UnsafeCell` and its pointers.

use shuttle::cell::{PtrRef, PtrRefMut, UnsafeCell};
use shuttle::sync::atomic::{AtomicBool, Ordering};
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_dfs, thread};
use std::rc::Rc;
use test_log::test;

/// A value that tasks share, with access only through the cell.
struct Shared<T>(UnsafeCell<T>);

// SAFETY: each test orders the accesses that this allows.
unsafe impl<T: Send> Sync for Shared<T> {}

#[test]
fn read_write_replace() {
    check_dfs(
        || {
            let cell = UnsafeCell::new(1u64);
            let ptr = cell.get();
            assert_eq!(unsafe { ptr.read() }, 1);
            unsafe { ptr.write(2) };
            assert_eq!(unsafe { ptr.replace(3) }, 2);
            assert_eq!(cell.into_inner(), 3);
        },
        None,
    );
}

#[test]
fn project_add_and_cast() {
    struct Pair {
        a: u32,
        b: u64,
    }

    check_dfs(
        || {
            let pair = UnsafeCell::new(Pair { a: 1, b: 2 });
            let b = pair.get().project(|p| unsafe { &raw mut (*p).b });
            unsafe { b.write(3) };
            let pair = pair.into_inner();
            assert_eq!((pair.a, pair.b), (1, 3));

            let array = UnsafeCell::new([10u32, 20, 30, 40]);
            let first = array.get().cast::<u32>();
            assert_eq!(unsafe { first.add(2).read() }, 30);
            assert_eq!(unsafe { first.byte_add(4).read() }, 20);
            unsafe { first.add(3).write(41) };
            assert_eq!(array.into_inner(), [10, 20, 30, 41]);
        },
        None,
    );
}

#[test]
fn references() {
    check_dfs(
        || {
            let cell = UnsafeCell::new(vec![1, 2]);
            {
                let mut v: PtrRefMut<'_, Vec<i32>> = unsafe { cell.get().as_mut() };
                v.push(3);
                assert_eq!(format!("{v:?}"), "[1, 2, 3]");
            }
            let r: PtrRef<'_, Vec<i32>> = unsafe { cell.get().as_ref() };
            let s = unsafe { cell.get().as_ref() };
            assert_eq!(*r, [1, 2, 3]);
            assert_eq!(s.len(), 3);
            assert_eq!(format!("{s:?}"), "[1, 2, 3]");

            let number = UnsafeCell::new(1u32);
            {
                let mut m = unsafe { number.get().as_mut() };
                unsafe { PtrRefMut::as_ptr(&mut m).write(2) };
            }
            let r = unsafe { number.get().as_ref() };
            assert_eq!(unsafe { PtrRef::as_ptr(&r).read() }, 2);
        },
        None,
    );
}

#[test]
fn loom_api() {
    check_dfs(
        || {
            let cell = UnsafeCell::new(5);
            cell.with_mut(|p| unsafe { *p += 1 });
            assert_eq!(cell.with(|p| unsafe { *p }), 6);
        },
        None,
    );
}

#[test]
fn get_mut_default_and_debug() {
    check_dfs(
        || {
            let mut cell = UnsafeCell::<u32>::default();
            *cell.get_mut() += 7;
            assert_eq!(unsafe { cell.get().read() }, 7);
            assert_eq!(format!("{cell:?}"), "UnsafeCell { .. }");
            assert!(format!("{:?}", cell.get()).starts_with("Ptr("));
        },
        None,
    );
}

#[test]
fn drops_its_value_once() {
    check_dfs(
        || {
            let value = Rc::new(());
            let cell = UnsafeCell::new(Rc::clone(&value));
            assert_eq!(Rc::strong_count(&value), 2);
            drop(cell);
            assert_eq!(Rc::strong_count(&value), 1);
        },
        None,
    );
}

#[test]
fn shared_under_a_mutex() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(()));
            let counter = Arc::new(Shared(UnsafeCell::new(0u64)));
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let lock = Arc::clone(&lock);
                    let counter = Arc::clone(&counter);
                    thread::spawn(move || {
                        let _guard = lock.lock().unwrap();
                        let mut value = unsafe { counter.0.get().as_mut() };
                        *value += 1;
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
            let _guard = lock.lock().unwrap();
            assert_eq!(unsafe { counter.0.get().read() }, 2);
        },
        None,
    );
}

#[test]
fn handed_off_by_an_atomic_flag() {
    check_dfs(
        || {
            let ready = Arc::new(AtomicBool::new(false));
            let slot = Arc::new(Shared(UnsafeCell::new(None)));
            let writer = {
                let ready = Arc::clone(&ready);
                let slot = Arc::clone(&slot);
                thread::spawn(move || {
                    unsafe { slot.0.get().write(Some(42)) };
                    ready.store(true, Ordering::Release);
                })
            };
            if ready.load(Ordering::Acquire) {
                assert_eq!(unsafe { slot.0.get().read() }, Some(42));
            }
            writer.join().unwrap();
        },
        None,
    );
}

#[test]
fn outside_an_execution() {
    let cell = UnsafeCell::new(1);
    unsafe { cell.get().write(2) };
    assert_eq!(unsafe { *cell.get().as_ref() }, 2);
    assert_eq!(cell.into_inner(), 2);
}

#[test]
fn is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<UnsafeCell<u32>>();
}
