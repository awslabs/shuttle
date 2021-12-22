use shuttle::scheduler::RandomScheduler;
use shuttle::{check, check_dfs, current, thread, Config, MaxSteps, Runner};
use std::panic::{catch_unwind, AssertUnwindSafe};
use test_log::test;
// Not actually trying to explore interleavings involving AtomicUsize, just using to smuggle a
// mutable counter across threads
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[test]
fn basic_scheduler_test() {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);

    check(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        let counter_clone = Arc::clone(&counter);
        thread::spawn(move || {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });
        counter.fetch_add(1, Ordering::SeqCst);
    });

    assert_eq!(counter_clone.load(Ordering::SeqCst), 3);
}

#[test]
fn max_steps_none() {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);

    let mut config = Config::new();
    config.max_steps = MaxSteps::None;

    let scheduler = RandomScheduler::new(10);
    let runner = Runner::new(scheduler, config);
    runner.run(move || {
        for _ in 0..100 {
            counter.fetch_add(1, Ordering::SeqCst);
            thread::yield_now();
        }
    });

    assert_eq!(counter_clone.load(Ordering::SeqCst), 100 * 10);
}

#[test]
fn max_steps_continue() {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);

    let mut config = Config::new();
    config.max_steps = MaxSteps::ContinueAfter(50);

    let scheduler = RandomScheduler::new(10);
    let runner = Runner::new(scheduler, config);
    runner.run(move || {
        for _ in 0..100 {
            counter.fetch_add(1, Ordering::SeqCst);
            thread::yield_now();
        }
    });

    assert_eq!(counter_clone.load(Ordering::SeqCst), 50 * 10);
}

#[test]
fn max_steps_fail() {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);

    let mut config = Config::new();
    config.max_steps = MaxSteps::FailAfter(50);

    let scheduler = RandomScheduler::new(10);
    let runner = Runner::new(scheduler, config);
    let result = catch_unwind(AssertUnwindSafe(move || {
        runner.run(move || {
            for _ in 0..100 {
                counter.fetch_add(1, Ordering::SeqCst);
                thread::yield_now();
            }
        })
    }));

    assert!(result.is_err());
    assert_eq!(counter_clone.load(Ordering::SeqCst), 50);
}

// Test that a scheduler can return `None` to trigger the same behavior as `MaxSteps::ContinueAfter`
#[test]
fn max_steps_early_exit_scheduler() {
    use shuttle::scheduler::{Schedule, Scheduler, TaskId};

    #[derive(Debug)]
    struct EarlyExitScheduler {
        iterations: usize,
        max_iterations: usize,
        steps: usize,
        max_steps: usize,
    }

    impl EarlyExitScheduler {
        fn new(max_iterations: usize, max_steps: usize) -> Self {
            Self {
                iterations: 0,
                max_iterations,
                steps: 0,
                max_steps,
            }
        }
    }

    impl Scheduler for EarlyExitScheduler {
        fn new_execution(&mut self) -> Option<Schedule> {
            if self.iterations >= self.max_iterations {
                None
            } else {
                self.iterations += 1;
                self.steps = 0;
                Some(Schedule::new(0))
            }
        }

        fn next_task(
            &mut self,
            runnable_tasks: &[TaskId],
            _current_task: Option<TaskId>,
            _is_yielding: bool,
        ) -> Option<TaskId> {
            if self.steps >= self.max_steps {
                None
            } else {
                self.steps += 1;
                Some(*runnable_tasks.first().unwrap())
            }
        }

        fn next_u64(&mut self) -> u64 {
            unimplemented!()
        }
    }

    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);

    let mut config = Config::new();
    config.max_steps = MaxSteps::FailAfter(51);

    let scheduler = EarlyExitScheduler::new(10, 50);
    let runner = Runner::new(scheduler, config);
    runner.run(move || {
        for _ in 0..100 {
            counter.fetch_add(1, Ordering::SeqCst);
            thread::yield_now();
        }
    });

    assert_eq!(counter_clone.load(Ordering::SeqCst), 50 * 10);
}

#[test]
#[should_panic]
fn context_switches_outside_execution() {
    current::context_switches();
}

#[test]
fn context_switches_atomic() {
    // The current implementation makes the following context switches:
    // 1 initial
    // 2 spawns
    // 2 joins
    // 2 thread terminations
    // 4 `fetch_add` (one before and one after each)
    const EXPECTED_CONTEXT_SWITCHES: usize = 11;

    check_dfs(
        move || {
            let mut threads = vec![];
            let counter = Arc::new(shuttle::sync::atomic::AtomicUsize::new(0));

            assert_eq!(current::context_switches(), 1);

            for _ in 0..2 {
                let counter = Arc::clone(&counter);

                threads.push(thread::spawn(move || {
                    let count = counter.fetch_add(1, Ordering::SeqCst) + 1;

                    // We saw the initial context switch, the spawn and first context switch for each `fetch_add`,
                    // and the second context switch after the `fetch_add` of this thread.
                    assert!(current::context_switches() >= 2 + 2 * count);

                    // We did not see the last context switch of this thread.
                    assert!(current::context_switches() < EXPECTED_CONTEXT_SWITCHES);
                }));
            }

            for thread in threads {
                thread.join().unwrap();
            }

            assert_eq!(current::context_switches(), EXPECTED_CONTEXT_SWITCHES);
        },
        None,
    );
}

#[test]
fn context_switches_mutex() {
    use shuttle::sync::Mutex;

    check_dfs(
        move || {
            let mutex1 = Arc::new(Mutex::new(0));
            let mutex2 = Arc::new(Mutex::new(0));

            assert_eq!(current::context_switches(), 1);

            {
                let mutex1 = mutex1.lock().unwrap();
                assert_eq!(current::context_switches(), 2);
                {
                    let mutex2 = mutex2.lock().unwrap();
                    assert_eq!(current::context_switches(), 3);
                    drop(mutex2);
                }
                assert_eq!(current::context_switches(), 4);
                drop(mutex1);
            }

            assert_eq!(current::context_switches(), 5);
        },
        None,
    );
}

/// Check that we get a good failure message if accessing a Shuttle primitive from outside an
/// execution.
#[test]
#[should_panic(expected = "are you trying to access a Shuttle primitive from outside a Shuttle test?")]
fn failure_outside_execution() {
    let lock = shuttle::sync::Mutex::new(0u64);
    let _ = lock.lock().unwrap();
}
