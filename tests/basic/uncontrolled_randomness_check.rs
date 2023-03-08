use shuttle::rand::{thread_rng, Rng};
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_uncontrolled_randomness, thread};
use test_log::test;

#[test]
fn randomly_acquire_lock_shuttle_rand() {
    check_uncontrolled_randomness(
        || {
            const NUM_THREADS: u32 = 10;

            let lock = Arc::new(Mutex::new(0u64));
            let threads: Vec<_> = (0..NUM_THREADS)
                .map(|_| {
                    let my_lock = lock.clone();

                    thread::spawn(move || {
                        let x = thread_rng().gen::<u64>();

                        // Fail every x threads
                        if x % 10 == 0 {
                            let mut num = my_lock.lock().unwrap();
                            *num += 1;
                        }
                    })
                })
                .collect();

            threads.into_iter().for_each(|t| t.join().expect("Failed"));
        },
        1000,
    );
}

#[test]
#[should_panic = "possible nondeterminism"]
fn randomly_acquire_lock_regular_rand() {
    check_uncontrolled_randomness(
        || {
            const NUM_THREADS: u32 = 10;

            let lock = Arc::new(Mutex::new(0u64));
            let threads: Vec<_> = (0..NUM_THREADS)
                .map(|_| {
                    let my_lock = lock.clone();

                    thread::spawn(move || {
                        let x = rand::thread_rng().gen::<u64>();

                        // Fail every x threads
                        if x % 10 == 0 {
                            let mut num = my_lock.lock().unwrap();
                            *num += 1;
                        }
                    })
                })
                .collect();

            threads.into_iter().for_each(|t| t.join().expect("Failed"));
        },
        1000,
    );
}

#[test]
fn spawn_100_threads() {
    shuttle::check_uncontrolled_randomness(
        || {
            let num_threads: u64 = 100;
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
        },
        100,
    );
}

#[test]
#[should_panic = "possible nondeterminism"]
fn spawn_random_amount_of_threads_regular_rand() {
    shuttle::check_uncontrolled_randomness(
        || {
            let num_threads: u64 = rand::thread_rng().gen::<u64>() % 10;
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
        },
        100,
    );
}

#[test]
fn spawn_random_amount_of_threads_shuttle_rand() {
    shuttle::check_uncontrolled_randomness(
        || {
            let num_threads: u64 = thread_rng().gen::<u64>() % 10;
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
        },
        100,
    );
}
