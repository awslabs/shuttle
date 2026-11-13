use shuttle::scheduler::{PosScheduler, Schedule, Scheduler, Task, TaskId};
use shuttle::sync::atomic::{AtomicBool, Ordering};
use shuttle::sync::Mutex;
use shuttle::{check_pos, check_random, thread, Config, Runner};
use std::sync::Arc;
use std::time::Duration;
use test_log::test;

const TEST_LENGTH: usize = 20;

/// Fig. 5 of the PCT paper (see `pct.rs`). The bug needs thread 1 to finish its `TEST_LENGTH` sleeps
/// and take the lock before the main thread does. A random walk does that with chance
/// `2^-(TEST_LENGTH + 1)`. The sleeps conflict with nothing, so POS keeps the main thread's lock
/// behind all of them with chance about `1/(TEST_LENGTH + 3)`.
fn figure5() {
    let lock = Arc::new(Mutex::new(0usize));
    let lock_clone = Arc::clone(&lock);

    thread::spawn(move || {
        for _ in 0..TEST_LENGTH {
            thread::sleep(Duration::from_millis(1));
        }

        *lock_clone.lock().unwrap() = 1;
    });

    let l = lock.lock().unwrap();
    assert_ne!(*l, 1, "thread 1 ran to completion");
}

#[test]
fn figure5_random() {
    // The random walk misses the bug with probability (1 - 2^-21)^1000 ~= 99.95%.
    check_random(figure5, 1000);
}

#[test]
#[should_panic(expected = "thread 1 ran to completion")]
fn figure5_pos() {
    // POS misses the bug with probability about (1 - 1/23)^1000 ~= 5e-20.
    check_pos(figure5, 1000);
}

/// Wraps a scheduler, and records the task it picks at every decision.
struct RecordingScheduler<S> {
    inner: S,
    choices: Arc<std::sync::Mutex<Vec<TaskId>>>,
}

impl<S: Scheduler> Scheduler for RecordingScheduler<S> {
    fn new_execution(&mut self) -> Option<Schedule> {
        self.inner.new_execution()
    }

    fn next_task(&mut self, runnable: &[&Task], current: Option<TaskId>, is_yielding: bool) -> Option<TaskId> {
        let choice = self.inner.next_task(runnable, current, is_yielding);
        self.choices.lock().unwrap().extend(choice);
        choice
    }

    fn next_u64(&mut self) -> u64 {
        self.inner.next_u64()
    }
}

fn many_tasks() {
    let lock = Arc::new(Mutex::new(0usize));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let lock = Arc::clone(&lock);
            thread::spawn(move || {
                for _ in 0..3 {
                    *lock.lock().unwrap() += 1;
                    thread::yield_now();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(*lock.lock().unwrap(), 12);
}

fn choices_from_seed(seed: u64) -> Vec<TaskId> {
    let choices = Arc::new(std::sync::Mutex::new(Vec::new()));
    let scheduler = RecordingScheduler {
        inner: PosScheduler::new_from_seed(seed, 20),
        choices: Arc::clone(&choices),
    };
    Runner::new(scheduler, Config::new()).run(many_tasks);
    let choices = choices.lock().unwrap();
    choices.clone()
}

#[test]
fn pos_is_deterministic_for_a_seed() {
    assert_eq!(choices_from_seed(42), choices_from_seed(42));
    assert_ne!(choices_from_seed(42), choices_from_seed(43));
}

#[test]
fn pos_runs_others_when_a_task_yields() {
    // The main thread spins until the other thread sets the flag. Without giving up its priority
    // when it yields, it could keep running for as long as its priority stays the highest.
    check_pos(
        || {
            let flag = Arc::new(AtomicBool::new(false));
            let setter = {
                let flag = Arc::clone(&flag);
                thread::spawn(move || flag.store(true, Ordering::SeqCst))
            };
            let mut spins = 0;
            while !flag.load(Ordering::SeqCst) {
                spins += 1;
                assert!(spins < 10, "the main thread kept running while it yielded");
                thread::yield_now();
            }
            setter.join().unwrap();
        },
        1000,
    );
}

#[test]
fn pos_without_decay_still_finishes() {
    let scheduler = PosScheduler::new(100).with_decay(0);
    Runner::new(scheduler, Config::new()).run(many_tasks);
}
