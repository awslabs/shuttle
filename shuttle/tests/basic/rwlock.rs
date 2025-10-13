use shuttle::scheduler::{PctScheduler, RoundRobinScheduler};
use shuttle::sync::{mpsc::channel, RwLock};
use shuttle::{check_dfs, check_random, thread, Runner};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, TryLockError};
use test_log::test;

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
    let runner = Runner::new(RoundRobinScheduler::new(1), Default::default());

    // Round-robin should always fail this deadlock test
    runner.run(deadlock);
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
    let scheduler = PctScheduler::new(2, 100);
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
    check_dfs(
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

    // Writer runs on the main thread
    let mut w = lock1.write().unwrap();
    *w += 1;
}

#[test]
fn rwlock_two_readers_and_one_writer_exhaustive() {
    check_dfs(two_readers_and_one_writer, None);
}

#[test]
fn rwlock_default() {
    struct Point(u32, u32);
    impl Default for Point {
        fn default() -> Self {
            Self(21, 42)
        }
    }

    check_dfs(
        || {
            let point: RwLock<Point> = Default::default();

            let r = point.read().unwrap();
            assert_eq!(r.0, 21);
            assert_eq!(r.1, 42);
        },
        None,
    );
}

#[test]
fn rwlock_into_inner() {
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(0u64));

            let threads = (0..2)
                .map(|_| {
                    let lock = lock.clone();
                    thread::spawn(move || {
                        *lock.write().unwrap() += 1;
                    })
                })
                .collect::<Vec<_>>();

            for thread in threads {
                thread.join().unwrap();
            }

            let lock = Arc::try_unwrap(lock).unwrap();
            assert_eq!(lock.into_inner().unwrap(), 2);
        },
        None,
    )
}

/// Two concurrent threads trying to do an atomic increment using `try_write`.
/// One `try_write` must succeed, while the other may or may not succeed.
/// Thus we expect to see final values 1 and 2.
#[test]
fn concurrent_try_increment() {
    let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
    let observed_values_clone = Arc::clone(&observed_values);

    check_dfs(
        move || {
            let lock = Arc::new(RwLock::new(0usize));

            let threads = (0..2)
                .map(|_| {
                    let lock = Arc::clone(&lock);
                    thread::spawn(move || {
                        match lock.try_write() {
                            Ok(mut guard) => {
                                *guard += 1;
                            }
                            Err(TryLockError::WouldBlock) => (),
                            Err(_) => panic!("unexpected TryLockError"),
                        };
                    })
                })
                .collect::<Vec<_>>();

            for thd in threads {
                thd.join().unwrap();
            }

            let value = Arc::try_unwrap(lock).unwrap().into_inner().unwrap();
            observed_values_clone.lock().unwrap().insert(value);
        },
        None,
    );

    let observed_values = Arc::try_unwrap(observed_values).unwrap().into_inner().unwrap();
    assert_eq!(observed_values, HashSet::from([1, 2]));
}

/// Run some threads that try to acquire the lock in both read and write modes, and check that any
/// execution allowed by our `RwLock` implementation is allowed by `std`.
#[test]
fn try_lock_implies_std() {
    check_random(
        move || {
            let lock = Arc::new(RwLock::new(()));
            let reference_lock = Arc::new(std::sync::RwLock::new(()));

            let threads = (0..3)
                .map(|_| {
                    let lock = Arc::clone(&lock);
                    let reference_lock = Arc::clone(&reference_lock);
                    thread::spawn(move || {
                        for _ in 0..3 {
                            {
                                let _r = lock.try_read();
                                if _r.is_ok() {
                                    assert!(reference_lock.try_read().is_ok());
                                }
                            }
                            {
                                let _w = lock.try_write();
                                if _w.is_ok() {
                                    assert!(reference_lock.try_write().is_ok());
                                }
                            }
                        }
                    })
                })
                .collect::<Vec<_>>();

            for thd in threads {
                thd.join().unwrap();
            }
        },
        5000,
    );
}

/// Run some threads that try to acquire the lock in both read and write modes, and check that any
/// execution allowed by `std` is allowed by our `RwLock` implementation. (This implication isn't
/// true in general -- see `double_try_read` -- but is true for this test).
#[test]
fn try_lock_implied_by_std() {
    check_random(
        move || {
            let lock = Arc::new(RwLock::new(()));
            let reference_lock = Arc::new(std::sync::RwLock::new(()));

            let threads = (0..3)
                .map(|_| {
                    let lock = Arc::clone(&lock);
                    let reference_lock = Arc::clone(&reference_lock);
                    thread::spawn(move || {
                        for _ in 0..5 {
                            {
                                let _r = reference_lock.try_read();
                                if _r.is_ok() {
                                    assert!(lock.try_read().is_ok());
                                }
                            }
                            {
                                let _w = reference_lock.try_write();
                                if _w.is_ok() {
                                    assert!(lock.try_write().is_ok());
                                }
                            }
                        }
                    })
                })
                .collect::<Vec<_>>();

            for thd in threads {
                thd.join().unwrap();
            }
        },
        5000,
    );
}

/// Three concurrent threads, one doing an atomic increment by 1 using `write`, one trying to do an
/// atomic increment by 1 followed by trying to do an atomic increment by 2 using `try_write`, and a
/// third that peeks at the value using `try_read.` The `write` must succeed, while each `try_write`
/// may or may not succeed.
#[test]
fn concurrent_write_try_write_try_read() {
    let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
    let observed_values_clone = Arc::clone(&observed_values);

    check_dfs(
        move || {
            let lock = Arc::new(RwLock::new(0usize));

            let write_thread = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    *lock.write().unwrap() += 1;
                })
            };
            let try_write_thread = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    for n in 1..3 {
                        match lock.try_write() {
                            Ok(mut guard) => {
                                *guard += n;
                            }
                            Err(TryLockError::WouldBlock) => (),
                            Err(_) => panic!("unexpected TryLockError"),
                        };
                    }
                })
            };

            let read_value = match lock.try_read() {
                Ok(guard) => Some(*guard),
                Err(TryLockError::WouldBlock) => None,
                Err(_) => panic!("unexpected TryLockError"),
            };

            write_thread.join().unwrap();
            try_write_thread.join().unwrap();

            let final_value = Arc::try_unwrap(lock).unwrap().into_inner().unwrap();
            observed_values_clone.lock().unwrap().insert((final_value, read_value));
        },
        None,
    );

    let observed_values = Arc::try_unwrap(observed_values).unwrap().into_inner().unwrap();
    // The idea here is that the `try_read` can interleave anywhere between the (successful) writes,
    // but can also just fail.
    let expected_values = HashSet::from([
        // Both `try_write`s fail
        (1, None),
        (1, Some(0)),
        (1, Some(1)),
        // Second `try_write` fails
        (2, None),
        (2, Some(0)),
        (2, Some(1)),
        (2, Some(2)),
        // First `try_write` fails
        (3, None),
        (3, Some(0)),
        (3, Some(1)),
        // (3, Some(2)), // If first `try_write` failed, the value of the lock is never 2
        (3, Some(3)),
        // Both `try_write`s succeed
        (4, None),
        (4, Some(0)),
        (4, Some(1)),
        (4, Some(2)),
        (4, Some(3)),
        (4, Some(4)),
    ]);
    assert_eq!(observed_values, expected_values);
}

/// This behavior is _sometimes_ allowed in `std`, but advised against, as it can lead to deadlocks
/// on some platforms if the second `read` races with another thread's `write`. We conservatively
/// rule it out in all cases to better detect potential deadlocks.
#[test]
#[should_panic(expected = "tried to acquire a RwLock it already holds")]
fn double_read() {
    check_dfs(
        || {
            let rwlock = RwLock::new(());
            let _guard_1 = rwlock.read().unwrap();
            let _guard_2 = rwlock.read();
        },
        None,
    )
}

#[test]
#[should_panic(expected = "tried to acquire a RwLock it already holds")]
fn double_write() {
    check_dfs(
        || {
            let rwlock = RwLock::new(());
            let _guard_1 = rwlock.write().unwrap();
            let _guard_2 = rwlock.write();
        },
        None,
    )
}

#[test]
#[should_panic(expected = "tried to acquire a RwLock it already holds")]
fn read_upgrade() {
    check_dfs(
        || {
            let rwlock = RwLock::new(());
            let _guard_1 = rwlock.read().unwrap();
            let _guard_2 = rwlock.write();
        },
        None,
    )
}

#[test]
#[should_panic(expected = "tried to acquire a RwLock it already holds")]
fn write_downgrade() {
    check_dfs(
        || {
            let rwlock = RwLock::new(());
            let _guard_1 = rwlock.write().unwrap();
            let _guard_2 = rwlock.read();
        },
        None,
    )
}

/// This behavior isn't consistent with `std`, which seems to suggest that `try_read` succeeds if
/// the current thread already holds a read lock. We assume it always fails so that we can more
/// easily diagnose potential deadlocks, especially with async tasks that might migrate across
/// threads in real implementations.
#[test]
fn double_try_read() {
    check_dfs(
        || {
            let rwlock = RwLock::new(());
            let _guard_1 = rwlock.try_read().unwrap();
            assert!(matches!(rwlock.try_read(), Err(TryLockError::WouldBlock)));
        },
        None,
    )
}

/// As with `double_try_read`, this isn't consistent with `std`.
#[test]
fn read_try_read() {
    check_dfs(
        || {
            let rwlock = RwLock::new(());
            let _guard_1 = rwlock.read().unwrap();
            assert!(matches!(rwlock.try_read(), Err(TryLockError::WouldBlock)));
        },
        None,
    )
}

#[test]
fn double_try_write() {
    check_dfs(
        || {
            let rwlock = RwLock::new(());
            let _guard_1 = rwlock.try_write().unwrap();
            assert!(matches!(rwlock.try_write(), Err(TryLockError::WouldBlock)));
        },
        None,
    )
}

#[test]
fn write_try_write() {
    check_dfs(
        || {
            let rwlock = RwLock::new(());
            let _guard_1 = rwlock.write().unwrap();
            assert!(matches!(rwlock.try_write(), Err(TryLockError::WouldBlock)));
        },
        None,
    )
}

#[test]
fn try_read_upgrade() {
    check_dfs(
        || {
            let rwlock = RwLock::new(());
            let _guard_1 = rwlock.try_read().unwrap();
            assert!(matches!(rwlock.try_write(), Err(TryLockError::WouldBlock)));
        },
        None,
    )
}

#[test]
fn try_write_downgrade() {
    check_dfs(
        || {
            let rwlock = RwLock::new(());
            let _guard_1 = rwlock.try_write().unwrap();
            assert!(matches!(rwlock.try_read(), Err(TryLockError::WouldBlock)));
        },
        None,
    )
}
