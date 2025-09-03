use shuttle::sync::{Mutex, RwLock};
use shuttle::{check_dfs, thread};
use std::panic::catch_unwind;
use std::sync::{Arc, PoisonError};
use test_log::test;

#[test]
fn mutex_poison() {
    check_dfs(
        || {
            let lock1 = Arc::new(Mutex::new(()));
            let lock1_clone = lock1.clone();

            let lock2 = Arc::new(Mutex::new(()));
            let _guard = lock2.lock();

            let jh = thread::spawn(move || {
                // panic while holding the mutex in order to poison it
                let _err = catch_unwind(move || {
                    let _lock = lock1_clone.lock().unwrap();
                    panic!();
                })
                .unwrap_err();
            });

            jh.join().unwrap();

            // lock1 should now be poisoned
            let result = lock1.lock();
            assert!(matches!(result, Err(PoisonError { .. })));

            drop(_guard);
            // lock2 should not be poisoned
            let lock_result = lock2.lock();
            assert!(lock_result.is_ok());
        },
        None,
    )
}

#[test]
fn rwlock_poison() {
    check_dfs(
        || {
            let lock1 = Arc::new(RwLock::new(()));
            let lock1_clone = lock1.clone();

            let lock2 = Arc::new(RwLock::new(()));
            let _guard = lock2.write();

            let jh = thread::spawn(move || {
                // panic while holding the mutex in order to poison it
                let _err = catch_unwind(move || {
                    let _lock = lock1_clone.write().unwrap();
                    panic!();
                })
                .unwrap_err();
            });

            jh.join().unwrap();

            // lock1 should now be poisoned
            let result = lock1.write();
            assert!(matches!(result, Err(PoisonError { .. })));

            drop(_guard);
            // lock2 should not be poisoned
            let lock_result = lock2.write();
            assert!(lock_result.is_ok());
        },
        None,
    )
}
