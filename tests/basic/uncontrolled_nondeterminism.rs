use rand::{rngs::StdRng, SeedableRng};
use shuttle::rand::Rng;
use shuttle::scheduler::{DfsScheduler, Scheduler};
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_uncontrolled_nondeterminism, thread};
use test_log::test;

fn check_uncontrolled_nondeterminism_custom_scheduler_and_config<F, S>(f: F, scheduler: S)
where
    F: Fn() + Send + Sync + 'static,
    S: Scheduler + 'static,
{
    use shuttle::scheduler::UncontrolledNondeterminismCheckScheduler;
    use shuttle::Config;

    let config = Config::default();

    let scheduler = UncontrolledNondeterminismCheckScheduler::new(scheduler);

    let runner = shuttle::Runner::new(scheduler, config);
    runner.run(f);
}

fn have_n_threads_acquire_mutex<R: rand::RngCore, F: (Fn() -> R) + Send + Sync>(
    thread_rng: &'static F,
    num_threads: u64,
    modulus: u64,
) {
    let lock = Arc::new(Mutex::new(0u64));
    let threads: Vec<_> = (0..num_threads)
        .map(|_| {
            let my_lock = lock.clone();

            thread::spawn(move || {
                let x = thread_rng().gen::<u64>();

                if x % modulus == 0 {
                    let mut num = my_lock.lock().unwrap();
                    *num += 1;
                }
            })
        })
        .collect();

    threads.into_iter().for_each(|t| t.join().expect("Failed"));
}

#[test]
fn randomly_acquire_lock_shuttle_rand() {
    check_uncontrolled_nondeterminism(
        || have_n_threads_acquire_mutex(&shuttle::rand::thread_rng, 10, 10),
        1000,
    );
}

#[test]
#[should_panic = "possible nondeterminism"]
fn randomly_acquire_lock_regular_rand() {
    check_uncontrolled_nondeterminism(|| have_n_threads_acquire_mutex(&rand::thread_rng, 10, 10), 1000);
}

fn spawn_random_amount_of_threads<R: rand::RngCore, F: (Fn() -> R) + Send + Sync>(
    thread_rng: &'static F,
    max_threads: u64,
) {
    let num_threads: u64 = thread_rng().gen::<u64>() % max_threads;
    have_n_threads_acquire_mutex(&rand::thread_rng, num_threads, 1);
}

#[test]
fn spawn_random_amount_of_threads_shuttle_rand() {
    check_uncontrolled_nondeterminism(|| spawn_random_amount_of_threads(&shuttle::rand::thread_rng, 10), 1000);
}

#[test]
#[should_panic = "possible nondeterminism"]
fn spawn_random_amount_of_threads_regular_rand() {
    check_uncontrolled_nondeterminism(|| spawn_random_amount_of_threads(&rand::thread_rng, 10), 1000);
}

#[test]
fn spawn_random_amount_of_threads_dfs_shuttle_rand() {
    let scheduler = DfsScheduler::new(None, true);
    check_uncontrolled_nondeterminism_custom_scheduler_and_config(
        || spawn_random_amount_of_threads(&shuttle::rand::thread_rng, 2),
        scheduler,
    );
}

#[test]
#[should_panic]
fn spawn_random_amount_of_threads_dfs_regular_rand() {
    let scheduler = DfsScheduler::new(None, true);
    check_uncontrolled_nondeterminism_custom_scheduler_and_config(
        || spawn_random_amount_of_threads(&rand::thread_rng, 10),
        scheduler,
    );
}

fn spawn_random_amount_of_threads_mutex_rng(rng: &Mutex<StdRng>, max_threads: u64) {
    let num_threads = rng.lock().unwrap().gen::<u64>() % max_threads;
    have_n_threads_acquire_mutex(&rand::thread_rng, num_threads, 1);
}

#[test]
#[should_panic = "possible nondeterminism: current execution should have ended"]
fn panic_should_have_ended() {
    let scheduler = DfsScheduler::new(None, true);
    let rng = Mutex::new(StdRng::seed_from_u64(123));
    check_uncontrolled_nondeterminism_custom_scheduler_and_config(
        move || spawn_random_amount_of_threads_mutex_rng(&rng, 2),
        scheduler,
    );
}

#[test]
#[should_panic = "possible nondeterminism: current execution ended earlier than expected"]
fn panic_ended_earlier() {
    let scheduler = DfsScheduler::new(None, true);
    let rng = Mutex::new(StdRng::seed_from_u64(123));
    check_uncontrolled_nondeterminism_custom_scheduler_and_config(
        move || spawn_random_amount_of_threads_mutex_rng(&rng, 3),
        scheduler,
    );
}

#[test]
#[should_panic = "possible nondeterminism: set of runnable tasks is different than expected"]
fn panic_set_of_runnable() {
    let scheduler = DfsScheduler::new(None, true);
    let rng = Mutex::new(StdRng::seed_from_u64(123));
    check_uncontrolled_nondeterminism_custom_scheduler_and_config(
        move || spawn_random_amount_of_threads_mutex_rng(&rng, 15),
        scheduler,
    );
}

fn make_random_numbers() {
    for _ in 0..10 {
        shuttle::rand::thread_rng().gen::<u64>();
    }
}

#[test]
#[should_panic = "possible nondeterminism: next step was context switch, but recording expected random number generation"]
fn panic_context_switch_when_expecting_rng() {
    let scheduler = DfsScheduler::new(None, true);
    let rng = Mutex::new(StdRng::seed_from_u64(123));
    let modulo = 3;
    check_uncontrolled_nondeterminism_custom_scheduler_and_config(
        move || {
            let test = rng.lock().unwrap().gen::<u64>() % modulo == 0;
            if test {
                have_n_threads_acquire_mutex(&rand::thread_rng, 10, 1);
            } else {
                make_random_numbers();
            }
        },
        scheduler,
    );
}

#[test]
#[should_panic = "possible nondeterminism: next step was random number generation, but recording expected context switch"]
fn panic_rng_when_expecting_context_switch() {
    let scheduler = DfsScheduler::new(None, true);
    let rng = Mutex::new(StdRng::seed_from_u64(123));
    let modulo = 5;
    check_uncontrolled_nondeterminism_custom_scheduler_and_config(
        move || {
            let test = rng.lock().unwrap().gen::<u64>() % modulo == 0;
            if test {
                have_n_threads_acquire_mutex(&rand::thread_rng, 10, 1);
            } else {
                make_random_numbers();
            }
        },
        scheduler,
    );
}

fn have_n_threads_yield(num_threads: u64) {
    let threads: Vec<_> = (0..num_threads)
        .map(|_| {
            thread::spawn(move || {
                thread::yield_now();
            })
        })
        .collect();

    threads.into_iter().for_each(|t| t.join().expect("Failed"));
}

#[test]
#[should_panic = "possible nondeterminism: `next_task` was called with `is_yielding`"]
fn panic_is_yielding() {
    let scheduler = DfsScheduler::new(None, true);
    let rng = Mutex::new(StdRng::seed_from_u64(123));
    let modulo = 5;
    check_uncontrolled_nondeterminism_custom_scheduler_and_config(
        move || {
            let test = rng.lock().unwrap().gen::<u64>() % modulo == 0;
            if test {
                have_n_threads_acquire_mutex(&rand::thread_rng, 10, 1);
            } else {
                have_n_threads_yield(10);
            }
        },
        scheduler,
    );
}

fn iterate_over_hash_set(num_entries: u64) {
    use std::collections::HashSet;
    let hash_set: HashSet<u64> = HashSet::from_iter(0..num_entries);
    for e in hash_set {
        if e % 2 == 0 {
            let _ = shuttle::rand::thread_rng().gen::<u64>();
        } else {
            let _ = thread::spawn(|| {}).join();
        }
    }
}

#[test]
#[should_panic = "possible nondeterminism: next step was"]
fn hashset_without_set_seed() {
    check_uncontrolled_nondeterminism(|| iterate_over_hash_set(10), 1000);
}

use shuttle::check_random;
use tracing::{warn, warn_span};

#[test]
fn tracing_nested_spans() {
    check_random(
        || {
            let lock = Arc::new(Mutex::new(0));
            let threads: Vec<_> = (0..3)
                .map(|i| {
                    let lock = lock.clone();
                    thread::spawn(move || {
                        let outer = warn_span!("outer", tid = i);
                        let _outer = outer.enter();
                        {
                            let mut locked = lock.lock().unwrap();
                            let inner = warn_span!("inner", tid = i);
                            let _inner = inner.enter();
                            warn!("incrementing from {}", *locked);
                            *locked += 1;
                        }
                    })
                })
                .collect();

            for thread in threads {
                thread.join().unwrap();
            }
        },
        10,
    )
}
