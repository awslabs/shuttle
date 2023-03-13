use shuttle::rand::Rng;
use shuttle::scheduler::{DfsScheduler, Scheduler};
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_uncontrolled_randomness, thread};
use test_log::test;

fn check_uncontrolled_randomness_custom_scheduler_and_config<F, S>(f: F, scheduler: S)
where
    F: Fn() + Send + Sync + 'static,
    S: Scheduler + 'static,
{
    use shuttle::scheduler::UncontrolledRandomnessCheckScheduler;
    use shuttle::Config;

    let config = Config::default();

    let scheduler = UncontrolledRandomnessCheckScheduler::new(scheduler);

    let runner = shuttle::Runner::new(scheduler, config);
    runner.run(f);
}

fn randomly_acquire_lock<F: (Fn() -> Box<dyn rand::RngCore>) + Send + Sync>(thread_rng: &'static F) {
    const NUM_THREADS: u32 = 10;

    let lock = Arc::new(Mutex::new(0u64));
    let threads: Vec<_> = (0..NUM_THREADS)
        .map(|_| {
            let my_lock = lock.clone();

            thread::spawn(move || {
                let x = thread_rng().gen::<u64>();

                if x % 10 == 0 {
                    let mut num = my_lock.lock().unwrap();
                    *num += 1;
                }
            })
        })
        .collect();

    threads.into_iter().for_each(|t| t.join().expect("Failed"));
}

fn spawn_random_amount_of_threads<F: (Fn() -> Box<dyn rand::RngCore>) + Send + Sync>(
    thread_rng: &'static F,
    max_threads: u64,
) {
    let num_threads: u64 = thread_rng().gen::<u64>() % max_threads;
    let lock = Arc::new(Mutex::new(0u64));
    let threads: Vec<_> = (0..num_threads)
        .map(|_| {
            let my_lock = lock.clone();

            thread::spawn(move || {
                let mut num = my_lock.lock().unwrap();
                *num += 1;
            })
        })
        .collect();

    threads.into_iter().for_each(|t| t.join().expect("Failed"));

    let num = lock.lock().unwrap();
    assert!(*num == num_threads);
}

#[test]
fn randomly_acquire_lock_shuttle_rand() {
    check_uncontrolled_randomness(
        || randomly_acquire_lock(&|| Box::new(shuttle::rand::thread_rng())),
        1000,
    );
}

#[test]
#[should_panic = "possible nondeterminism"]
fn randomly_acquire_lock_regular_rand_new() {
    check_uncontrolled_randomness(|| randomly_acquire_lock(&|| Box::new(rand::thread_rng())), 1000);
}

#[test]
fn spawn_random_amount_of_threads_shuttle_rand() {
    check_uncontrolled_randomness(
        || spawn_random_amount_of_threads(&|| Box::new(shuttle::rand::thread_rng()), 10),
        1000,
    );
}

#[test]
#[should_panic = "possible nondeterminism"]
fn spawn_random_amount_of_threads_regular_rand() {
    check_uncontrolled_randomness(
        || spawn_random_amount_of_threads(&|| Box::new(rand::thread_rng()), 10),
        1000,
    );
}

#[test]
fn spawn_random_amount_of_threads_dfs_shuttle_rand() {
    let scheduler = DfsScheduler::new(None, true);
    check_uncontrolled_randomness_custom_scheduler_and_config(
        || spawn_random_amount_of_threads(&|| Box::new(shuttle::rand::thread_rng()), 2),
        scheduler,
    );
}

#[test]
#[should_panic]
fn spawn_random_amount_of_threads_dfs_regular_rand() {
    let scheduler = DfsScheduler::new(None, true);
    check_uncontrolled_randomness_custom_scheduler_and_config(
        || spawn_random_amount_of_threads(&|| Box::new(rand::thread_rng()), 10),
        scheduler,
    );
}
