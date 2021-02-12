use shuttle::scheduler::RandomScheduler;
use shuttle::sync::Mutex;
use shuttle::{thread, Config, Runner};
use std::sync::Arc;

fn check_max_tasks(max_tasks: usize, num_spawn: usize) {
    let config = Config {
        max_tasks,
        ..Default::default()
    };

    let scheduler = RandomScheduler::new(100);
    let runner = Runner::new(scheduler, config);
    runner.run(move || {
        let lock = Arc::new(Mutex::new(()));
        for _ in 0..num_spawn {
            let mlock = Arc::clone(&lock);
            thread::spawn(move || {
                mlock.lock().unwrap();
            });
        }
    });
}

#[test]
#[should_panic(expected = "index out of bounds")]
fn max_task_fail() {
    check_max_tasks(5, 5); // initial thread adds 1
}

#[test]
fn max_task_ok() {
    check_max_tasks(5, 4);
}

#[test]
#[ignore] // Crashes with SIGBUS if you run it
fn max_stack_depth() {
    let stack_size = 1024usize;
    let config = Config {
        stack_size,
        ..Default::default()
    };
    let scheduler = RandomScheduler::new(100);
    let runner = Runner::new(scheduler, config);
    runner.run(|| {
        thread::spawn(|| {
            let v = vec![99; 1024];
            assert_eq!(v[0], 99);
        });
    });
}
