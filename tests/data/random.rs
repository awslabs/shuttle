use crate::check_replay_roundtrip;
use shuttle::rand::{thread_rng, Rng};
use shuttle::scheduler::RandomScheduler;
use shuttle::sync::Mutex;
use shuttle::{check_random, replay, thread};
use shuttle::{scheduler::DfsScheduler, Runner};
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use test_log::test;

fn random_mod_10_equals_7() {
    let mut rng = thread_rng();
    let x = rng.gen::<u64>();
    assert_ne!(x % 10, 7, "found failing value {}", x);
}

#[test]
#[should_panic(expected = "found failing value")]
fn random_mod_10_equals_7_fails() {
    check_random(random_mod_10_equals_7, 1000)
}

#[test]
#[should_panic(expected = "found failing value")]
fn random_mod_10_equals_7_replay_fails() {
    // A schedule in which the random value is 12690273488315200547 == 7 mod 10
    replay(random_mod_10_equals_7, "910102fe93a9cef4f3faaf5a04")
}

#[test]
fn random_mod_10_equals_7_replay_succeeds() {
    // A schedule in which the random value is 8809595901112014164 != 7 mod 10
    replay(random_mod_10_equals_7, "910102d2c4cfddfba9a4f2c80104")
}

#[test]
fn random_mod_10_equals_7_replay_roundtrip() {
    check_replay_roundtrip(random_mod_10_equals_7, RandomScheduler::new(1000))
}

// Check that multiple threads using `thread_rng` aren't seeing the same stream of randomness
fn thread_rng_decorrelated() {
    let lock = Arc::new(Mutex::new([0, 0]));

    let thds = (0..2)
        .map(|i| {
            let lock = lock.clone();
            thread::spawn(move || {
                let mut rng = thread_rng();
                lock.lock().unwrap()[i] = rng.gen::<usize>();
            })
        })
        .collect::<Vec<_>>();

    for thd in thds {
        thd.join().unwrap();
    }

    let arr = lock.lock().unwrap();
    assert_ne!(arr[0], arr[1]);
}

#[test]
fn random_thread_rng_decorrelated() {
    check_random(thread_rng_decorrelated, 1000)
}

// Check that `thread_rng` uses a different seed on every execution
#[test]
fn random_reseeds_on_every_execution() {
    let set = std::sync::Arc::new(std::sync::Mutex::new(HashSet::new()));
    let set_clone = set.clone();

    check_random(
        move || {
            let mut rng = thread_rng();
            let x = rng.gen::<u64>();
            set.lock().unwrap().insert(x);
        },
        1000,
    );

    let set = set_clone.lock().unwrap();
    assert!(set.len() >= 800);
}

#[derive(Debug)]
struct BrokenAtomicCounter {
    counter: Mutex<usize>,
}

impl BrokenAtomicCounter {
    fn new() -> Self {
        Self { counter: Mutex::new(0) }
    }

    // inc works fine
    fn inc(&self) {
        let mut counter = self.counter.lock().unwrap();
        *counter = counter.wrapping_add(1);
    }

    // dec is broken (not atomic)
    fn dec(&self) {
        let x = *self.counter.lock().unwrap();
        let y = x.wrapping_sub(1);
        *self.counter.lock().unwrap() = y;
    }
}

// A test that combines data nondeterminism (to choose actions) with schedule nondeterminism
// (the bug only occurs if a task is preempted during `dec`)
fn broken_atomic_counter_stress() {
    let counter = Arc::new(BrokenAtomicCounter::new());
    let truth = Arc::new(AtomicUsize::new(0));

    let num_threads = thread_rng().gen_range(1usize..4);
    let threads = (0..num_threads)
        .map(|_| {
            let counter = counter.clone();
            let truth = truth.clone();

            thread::spawn(move || {
                let mut rng = thread_rng();
                let num_steps = rng.gen_range(1..10);
                for _ in 0..num_steps {
                    if rng.gen::<bool>() {
                        counter.inc();
                        truth.fetch_add(1, Ordering::SeqCst);
                    } else {
                        counter.dec();
                        truth.fetch_sub(1, Ordering::SeqCst); // fetch_sub wraps on underflow
                    }
                }
            })
        })
        .collect::<Vec<_>>();

    for thd in threads {
        thd.join().unwrap();
    }

    let counter = *Arc::try_unwrap(counter).unwrap().counter.lock().unwrap();
    let truth = Arc::try_unwrap(truth).unwrap().into_inner();
    assert_eq!(counter, truth, "counter is incorrect");
}

#[test]
#[should_panic(expected = "counter is incorrect")]
fn broken_atomic_counter_stress_random() {
    check_random(broken_atomic_counter_stress, 1000)
}

#[test]
fn broken_atomic_counter_stress_roundtrip() {
    check_replay_roundtrip(broken_atomic_counter_stress, RandomScheduler::new(1000))
}

#[test]
#[should_panic(expected = "requested random data from DFS scheduler")]
fn dfs_thread_rng_decorrelated_disabled() {
    let scheduler = DfsScheduler::new(None, false);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(thread_rng_decorrelated);
}

#[test]
fn dfs_threads_decorrelated_enabled() {
    let scheduler = DfsScheduler::new(None, true);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(thread_rng_decorrelated);
}

// The DFS scheduler uses the same stream of randomness on each execution to ensure determinism
#[test]
fn dfs_does_not_reseed_across_executions() {
    let pair = std::sync::Arc::new(std::sync::Mutex::new((HashSet::new(), 0usize)));
    let pair_clone = pair.clone();

    let scheduler = DfsScheduler::new(None, true);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(move || {
        thread::spawn(|| {
            for _ in 0..3 {
                thread::yield_now();
            }
        });

        let mut rng = thread_rng();
        let x = rng.gen::<u64>();
        let mut set = pair.lock().unwrap();
        set.0.insert(x);
        set.1 += 1;
    });

    let set = pair_clone.lock().unwrap();
    assert!(set.1 > 1, "should have executed the test more than once");
    assert_eq!(set.0.len(), 1, "should only see one random value");
}
