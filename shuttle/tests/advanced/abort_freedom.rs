use shuttle::future::batch_semaphore::BatchSemaphore;
use shuttle::scheduler::DfsScheduler;
use shuttle::sync::Mutex;
use shuttle::{Config, Runner};
use std::collections::VecDeque;
use std::sync::Arc;

// Similar to the test below. Tests that setting `immediately_return_on_panic` makes what would otherwise be an abort
// into a oanic.
#[test]
// A drawback of the way this is done is that the panic payload is lost (thought it's still printed to stderr)
#[should_panic(expected = "Task panicked, and early return is enabled.")]
fn panic_handling_avoids_aborting() {
    let scheduler = DfsScheduler::new(None, false);

    let mut config = Config::default();
    config.ungraceful_shutdown_config.immediately_return_on_panic = true;

    let runner = Runner::new(scheduler, config);

    runner.run(|| {
        let _panic_on_drop = PanicOnDrop {};
        panic!("Cat goes purr");
    });
}
struct PanicOnDrop {}

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        shuttle::thread::yield_now();
        panic!("PanicOnDrop dropped");
    }
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

#[test]
#[should_panic(expected = "Panic message")]
fn panic_handling_moved_function_gets_leaked() {
    let scheduler = DfsScheduler::new(None, false);

    let config = Config::default();

    let runner = Runner::new(scheduler, config);

    runner.run(|| {
        let _panic_on_drop = PanicOnDrop {};
        shuttle::thread::spawn(move || {
            let _panic_on_drop = _panic_on_drop;
            // We block ourself. `_panic_on_drop` won't be dropped until we hit the panic
            // in the main thread.
            let semaphore = BatchSemaphore::new(0, shuttle::future::batch_semaphore::Fairness::StrictlyFair);
            semaphore.acquire_blocking(1).unwrap();
        });
        panic!("Panic message");
    });
}
