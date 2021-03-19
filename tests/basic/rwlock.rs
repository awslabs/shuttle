use shuttle::scheduler::PCTScheduler;
use shuttle::sync::{mpsc::channel, RwLock};
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
    let runner = Runner::new(scheduler, Default::default());
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

// Check that multiple readers are allowed to read at a time
// This test should never deadlock.
#[test]
fn rwlock_allows_multiple_readers() {
    shuttle::check_dfs(
        || {
            let lock1 = Arc::new(RwLock::new(1));
            let lock2 = lock1.clone();

            let (s1, r1) = channel::<usize>();
            let (s2, r2) = channel::<usize>();

            thread::spawn(move || {
                let w = lock1.read().unwrap();
                s1.send(*w).unwrap(); // Send value to other thread
                let r = r2.recv().unwrap(); // Wait for value from other thread
                assert_eq!(r, 1);
            });

            thread::spawn(move || {
                let w = lock2.read().unwrap();
                s2.send(*w).unwrap();
                let r = r1.recv().unwrap();
                assert_eq!(r, 1);
            });
        },
        None,
        None,
    );
}

// Ensure that a writer is never woken up when a reader is using the lock
fn two_readers_and_one_writer() {
    let lock1 = Arc::new(RwLock::new(1));

    // Spawn two readers, each tries to acquire both locks
    for _ in 0..2 {
        let rlock1 = lock1.clone();
        thread::spawn(move || {
            let r1 = rlock1.read().unwrap();
            thread::yield_now();
            assert!(*r1 > 0);
        });
    }

    thread::spawn(move || {
        let mut w = lock1.write().unwrap();
        *w += 1;
    });
}

#[test]
fn rwlock_two_readers_and_one_writer_exhaustive() {
    shuttle::check_dfs(
        || {
            two_readers_and_one_writer();
        },
        None,
        None,
    );
}

#[test]
fn rwlock_default() {
    struct Point(u32, u32);
    impl Default for Point {
        fn default() -> Self {
            Self(21, 42)
        }
    }

    shuttle::check_dfs(
        || {
            let point: RwLock<Point> = Default::default();

            let r = point.read().unwrap();
            assert_eq!(r.0, 21);
            assert_eq!(r.1, 42);
        },
        None,
        None,
    );
}