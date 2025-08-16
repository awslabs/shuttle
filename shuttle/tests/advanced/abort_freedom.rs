use shuttle::check_random;
use shuttle::scheduler::DfsScheduler;
use shuttle::sync::Mutex;
use shuttle::{check_dfs, Config, Runner};
use std::collections::VecDeque;
use std::sync::Arc;

fn query_a() -> u32 {
    query_b()
}

fn query_b() -> u32 {
    let _p = PanicOnDrop {};
    panic!("cancel!")
}

#[test]
fn execute() {
    check_random(
        || {
            use shuttle::thread::spawn;

            let t1 = spawn(query_a);
            let t2 = spawn(query_b);

            // The main thing here is that we don't deadlock.
            let (_r1, _r2) = (t1.join(), t2.join());
        },
        100,
    );
}

// Similar to the test below. Tests that we
#[test]
fn meow() {
    check_random(
        || {
            let _panic_on_drop = PanicOnDrop {};
            panic!("Cat goes purr");
        },
        100,
    );
}
struct PanicOnDrop {}

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        //shuttle::thread::yield_now();
        panic!("PanicOnDrop dropped");
    }
}

// Similar to the test below. Tests that we
#[should_panic(expected = "Cat goes purr")]
#[test]
#[ignore] // test aborts. Can be made non-aborting
fn panic_handling_avoids_aborting() {
    check_dfs(
        || {
            let _panic_on_drop = PanicOnDrop {};
            panic!("Cat goes purr");
        },
        None,
    );
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
