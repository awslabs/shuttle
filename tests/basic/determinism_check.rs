// use crate::check_replay_roundtrip;
use shuttle::rand::{thread_rng, Rng};
// use shuttle::scheduler::RandomScheduler;
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_determinism, thread};
use test_log::test;

#[test]
#[should_panic]
fn shuttle_determinism_test_randomly_acquire_lock() {
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

            let num = lock.lock().unwrap();
            println!("Lock accessed {} times", num);
        },
        1000,
        10,
    );
}

#[test]
#[should_panic]
fn shuttle_determinism_test_spawn_random_amount_of_threads() {
    shuttle::check_determinism(
        || {
            // Should fail
            let num_threads: u64 = thread_rng().gen::<u64>() % 100;
            println!("Number of threads: {}", num_threads);
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
fn shuttle_determinism_test_spawn_100_threads() {
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
