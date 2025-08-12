use shuttle::scheduler::DfsScheduler;
use shuttle::sync::{Mutex, PoisonError, RwLock};
use shuttle::{check_dfs, thread, Config, Runner};
use std::collections::VecDeque;
use std::panic::catch_unwind;
use std::sync::Arc;
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

// This test generates a panic that poisons a lock, and then while unwinding due to that panic, runs
// a Drop handler that tries to acquire that same lock. That leads to a double panic, which aborts
// the process. Since this is a common pattern in Rust, we want to check that we at least get a
// usable schedule printed when this happens.
#[test]
#[ignore] // tests a double panic, so we can't enable it by default
fn max_steps_panic_during_drop() {
    let config = Config::new();
    let scheduler = DfsScheduler::new(None, false);
    let runner = Runner::new(scheduler, config);
    runner.run(|| {
        #[derive(Clone)]
        struct Pool {
            items: Arc<Mutex<VecDeque<usize>>>,
        }

        struct PoolItem {
            pool: Arc<Mutex<VecDeque<usize>>>,
            item: usize,
        }

        impl Pool {
            fn new(length: usize) -> Self {
                Self {
                    items: Arc::new(Mutex::new((0..length).collect())),
                }
            }

            fn get(&self) -> PoolItem {
                let mut items = self.items.lock().unwrap();
                let item = items.pop_front().unwrap();
                PoolItem {
                    pool: self.items.clone(),
                    item,
                }
            }
        }

        impl Drop for PoolItem {
            fn drop(&mut self) {
                let mut items = self.pool.lock().unwrap();
                items.push_back(self.item);
            }
        }

        let pool = Pool::new(1);

        let _item1 = pool.get();
        let _item2 = pool.get();
    });
}
