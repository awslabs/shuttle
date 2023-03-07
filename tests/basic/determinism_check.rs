use shuttle::rand::{thread_rng, Rng};
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_determinism, thread};
use test_log::test;

#[test]
#[should_panic]
fn randomly_acquire_lock() {
    check_determinism(
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
        10,
    );
}

#[test]
#[should_panic(expected = "possible nondeterminism: set of runnable tasks is different than expected")]
fn spawn_random_amount_of_threads() {
    shuttle::check_determinism(
        || {
            // Should fail
            let num_threads: u64 = thread_rng().gen::<u64>() % 100;
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
        10,
    );
}

#[test]
fn spawn_100_threads() {
    shuttle::check_determinism(
        || {
            // Should pass
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
        10,
    );
}

#[test]
fn spawn_random_amount_of_threads2() {
    shuttle::check_determinism(
        || {
            let num_threads = shuttle::rand::thread_rng().gen_range(1..10);
            let lock = Arc::new(Mutex::new(0u64));
            let threads: Vec<_> = (0..num_threads)
                .map(|_| {
                    let my_lock = lock.clone();
                    thread::spawn(move || {
                        *my_lock.lock().unwrap() += 1;
                    })
                })
                .collect();
            threads.into_iter().for_each(|t| t.join().expect("Failed"));
            let num = lock.lock().unwrap();
            assert!(*num == num_threads);
        },
        100,
        10,
    );
}
