use shuttle::scheduler::RandomScheduler;
use shuttle::sync::Mutex;
use shuttle::{thread, Config, Runner};
use std::sync::Arc;

#[test]
fn many_tasks_with_mutex() {
    let num_spawn = 1000;
    let config = Config::new();
    let scheduler = RandomScheduler::new(10);
    let runner = Runner::new(scheduler, config);
    runner.run(move || {
        let count = Arc::new(Mutex::new(0));
        let handles = (0..num_spawn)
            .map(|_| {
                let count = Arc::clone(&count);
                thread::spawn(move || {
                    *count.lock().unwrap() += 1;
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(*count.lock().unwrap(), num_spawn);
    });
}

#[test]
#[ignore] // Crashes with SIGBUS if you run it
fn max_stack_depth() {
    let mut config = Config::new();
    config.stack_size = 1024;

    let scheduler = RandomScheduler::new(100);
    let runner = Runner::new(scheduler, config);
    runner.run(|| {
        thread::spawn(|| {
            let v = vec![99; 1024];
            assert_eq!(v[0], 99);
        });
    });
}
