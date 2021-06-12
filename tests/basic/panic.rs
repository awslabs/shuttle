use shuttle::sync::{Mutex, RwLock};
use shuttle::{check_dfs, thread};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, PoisonError};
use test_env_log::test;

#[test]
fn mutex_poison() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(false));

            let thd = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let _err = catch_unwind(AssertUnwindSafe(move || {
                        let _lock = lock.lock().unwrap();
                        panic!("expected panic");
                    }))
                    .unwrap_err();
                })
            };

            thd.join().unwrap();

            let result = lock.lock();
            assert!(matches!(result, Err(PoisonError { .. })));
        },
        None,
    )
}

#[test]
fn rwlock_poison() {
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(false));

            let thd = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let _err = catch_unwind(AssertUnwindSafe(move || {
                        let _lock = lock.write().unwrap();
                        panic!("expected panic");
                    }))
                    .unwrap_err();
                })
            };

            thd.join().unwrap();

            let result = lock.read();
            assert!(matches!(result, Err(PoisonError { .. })));
        },
        None,
    )
}
