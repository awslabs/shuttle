use shuttle::scheduler::{RandomScheduler, Schedule, Scheduler, Task, TaskId};
use shuttle::sync::Mutex;
use shuttle::Config;
use shuttle::{thread, Runner};
use std::sync::Arc;
use std::time::Duration;
use test_log::test;

/// A scheduler that sleeps between executions to test timeout behavior
#[derive(Debug)]
struct SleepableScheduler<S> {
    scheduler: S,
    sleep_time: Duration,
}

impl<S: Scheduler> SleepableScheduler<S> {
    fn new(scheduler: S, sleep_time: Duration) -> Self {
        Self { scheduler, sleep_time }
    }
}

impl<S: Scheduler> Scheduler for SleepableScheduler<S> {
    fn new_execution(&mut self) -> Option<Schedule> {
        std::thread::sleep(self.sleep_time);
        self.scheduler.new_execution()
    }

    fn next_task(
        &mut self,
        runnable_tasks: &[&Task],
        current_task: Option<TaskId>,
        is_yielding: bool,
    ) -> Option<TaskId> {
        self.scheduler.next_task(runnable_tasks, current_task, is_yielding)
    }

    fn next_u64(&mut self) -> u64 {
        self.scheduler.next_u64()
    }
}

#[test]
fn runner_timeout() {
    let scheduler = RandomScheduler::new(100000);
    let scheduler = SleepableScheduler::new(scheduler, Duration::from_millis(10));

    let mut config = Config::new();
    config.max_time = Some(Duration::from_secs(1));

    let runner = Runner::new(scheduler, config);

    let iterations = runner.run(|| {
        let lock = Arc::new(Mutex::new(0usize));
        let lock_clone = Arc::clone(&lock);

        thread::spawn(move || {
            let mut counter = lock_clone.lock().unwrap();
            *counter += 1;
        });

        let mut counter = lock.lock().unwrap();
        *counter += 1;
    });

    assert!(iterations < 10000, "test must stop well before max_iterations");
    assert!(
        iterations >= 1,
        "test must run at least once (maybe running on a _very_ slow host?"
    );
}
