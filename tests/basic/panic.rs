use shuttle::scheduler::DfsScheduler;
use shuttle::sync::{Mutex, RwLock};
use shuttle::{check_dfs, thread, Config, Runner};
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, PoisonError};
use test_log::test;

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
