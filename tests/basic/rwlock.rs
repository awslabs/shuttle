use shuttle::scheduler::PCTScheduler;
use shuttle::sync::RwLock;
use shuttle::{check, check_random, thread, Runner};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use test_env_log::test;

#[test]
fn reader_concurrency() {
    let saw_concurrent_reads = Arc::new(AtomicBool::new(false));

    {
        let saw_concurrent_reads = Arc::clone(&saw_concurrent_reads);

        check_random(
            move || {
                let rwlock = Arc::new(RwLock::new(0usize));
                let readers = Arc::new(AtomicUsize::new(0));

                {
                    let rwlock = Arc::clone(&rwlock);
                    let readers = Arc::clone(&readers);
                    let saw_concurrent_reads = Arc::clone(&saw_concurrent_reads);

                    thread::spawn(move || {
                        let counter = rwlock.read().unwrap();
                        assert_eq!(*counter, 0);

                        readers.fetch_add(1, Ordering::SeqCst);

                        thread::yield_now();

                        if readers.load(Ordering::SeqCst) == 2 {
                            saw_concurrent_reads.store(true, Ordering::SeqCst);
                        }

                        readers.fetch_sub(1, Ordering::SeqCst);
                    });
                }

                let counter = rwlock.read().unwrap();
                assert_eq!(*counter, 0);

                readers.fetch_add(1, Ordering::SeqCst);

                thread::yield_now();

                if readers.load(Ordering::SeqCst) == 2 {
                    saw_concurrent_reads.store(true, Ordering::SeqCst);
                }

                readers.fetch_sub(1, Ordering::SeqCst);
            },
            100,
        );
    }

    assert!(saw_concurrent_reads.load(Ordering::SeqCst));
}

fn deadlock() {
    let lock1 = Arc::new(RwLock::new(0usize));
    let lock2 = Arc::new(RwLock::new(0usize));
    let lock1_clone = Arc::clone(&lock1);
    let lock2_clone = Arc::clone(&lock2);

    thread::spawn(move || {
        let _l1 = lock1_clone.read().unwrap();
        let _l2 = lock2_clone.read().unwrap();
    });

    let _l2 = lock2.write().unwrap();
    let _l1 = lock1.write().unwrap();
}

#[test]
#[should_panic(expected = "deadlock")]
fn deadlock_default() {
    // Round-robin should always fail this deadlock test
    check(deadlock);
}

#[test]
#[should_panic(expected = "deadlock")]
fn deadlock_random() {
    // 200 tries should be enough to find a deadlocking execution
    check_random(deadlock, 200);
}

#[test]
#[should_panic(expected = "deadlock")]
fn deadlock_pct() {
    // 200 tries should be enough to find a deadlocking execution
    let scheduler = PCTScheduler::new(2, 100);
    let runner = Runner::new(scheduler);
    runner.run(deadlock);
}

// Test case for a bug we found in Loom: https://github.com/tokio-rs/loom/pull/135
#[test]
fn rwlock_two_writers() {
    check_random(
        || {
            let lock = Arc::new(RwLock::new(1));
            let lock2 = lock.clone();

            thread::spawn(move || {
                let mut w = lock.write().unwrap();
                *w += 1;
                thread::yield_now();
            });

            thread::spawn(move || {
                let mut w = lock2.write().unwrap();
                *w += 1;
                thread::yield_now();
            });
        },
        100,
    );
}
