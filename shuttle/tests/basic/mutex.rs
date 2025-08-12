use shuttle::scheduler::PctScheduler;
use shuttle::sync::{Mutex, TryLockError};
use shuttle::{check_dfs, check_random, thread, Runner};
use std::collections::HashSet;
use std::sync::Arc;
use test_log::test;

#[test]
fn basic_lock_test() {
    check_dfs(
        move || {
            let lock = Arc::new(Mutex::new(0usize));
            let lock_clone = Arc::clone(&lock);

            thread::spawn(move || {
                let mut counter = lock_clone.lock().unwrap();
                *counter += 1;
            });

            let mut counter = lock.lock().unwrap();
            *counter += 1;
        },
        None,
    );

    // TODO would be cool if we were allowed to smuggle the lock out of the run,
    // TODO so we can assert invariants about it *after* execution ends
}

// TODO we need a test that actually validates mutual exclusion? or is `deadlock` enough?

fn deadlock() {
    let lock1 = Arc::new(Mutex::new(0usize));
    let lock2 = Arc::new(Mutex::new(0usize));
    let lock1_clone = Arc::clone(&lock1);
    let lock2_clone = Arc::clone(&lock2);

    thread::spawn(move || {
        let _l1 = lock1_clone.lock().unwrap();
        let _l2 = lock2_clone.lock().unwrap();
    });

    let _l2 = lock2.lock().unwrap();
    let _l1 = lock1.lock().unwrap();
}

#[test]
#[should_panic(expected = "deadlock")]
fn deadlock_default() {
    check_dfs(deadlock, None);
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
    // 100 tries should be enough to find a deadlocking execution
    let scheduler = PctScheduler::new(2, 100);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(deadlock);
}

#[test]
#[should_panic(expected = "racing increments")]
fn concurrent_increment_buggy() {
    let scheduler = PctScheduler::new(2, 100);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(|| {
        let lock = Arc::new(Mutex::new(0usize));

        let threads = (0..2)
            .map(|_| {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let curr = *lock.lock().unwrap();
                    *lock.lock().unwrap() = curr + 1;
                })
            })
            .collect::<Vec<_>>();

        for thd in threads {
            thd.join().unwrap();
        }

        let counter = *lock.lock().unwrap();
        assert_eq!(counter, 2, "racing increments");
    });
}

#[test]
fn concurrent_increment() {
    let scheduler = PctScheduler::new(2, 100);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(|| {
        let lock = Arc::new(Mutex::new(0usize));

        let threads = (0..2)
            .map(|_| {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    *lock.lock().unwrap() += 1;
                })
            })
            .collect::<Vec<_>>();

        for thd in threads {
            thd.join().unwrap();
        }

        assert_eq!(*lock.lock().unwrap(), 2);
    });
}

/// Two concurrent threads, one doing two successive atomic increments, the other doing
/// an atomic multiplication by 2. The multiplication can go before, between, and after
/// the increments. Thus we expect to see final values
/// * 2 (`0 * 2 + 1 + 1`),
/// * 3 (`(0 + 1) * 2 + 1`), and
/// * 4 (`(0 + 1 + 1) * 2`).
#[test]
fn unlock_yields() {
    let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
    let observed_values_clone = Arc::clone(&observed_values);

    check_dfs(
        move || {
            let lock = Arc::new(Mutex::new(0usize));

            let add_thread = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    *lock.lock().unwrap() += 1;
                    *lock.lock().unwrap() += 1;
                })
            };
            let mul_thread = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    *lock.lock().unwrap() *= 2;
                })
            };

            add_thread.join().unwrap();
            mul_thread.join().unwrap();

            let value = Arc::try_unwrap(lock).unwrap().into_inner().unwrap();
            observed_values_clone.lock().unwrap().insert(value);
        },
        None,
    );

    let observed_values = Arc::try_unwrap(observed_values).unwrap().into_inner().unwrap();
    assert_eq!(observed_values, HashSet::from([2, 3, 4]));
}

/// Similar to `unlock_yields`, but testing some interaction of `Mutex` with `RwLock`.
#[test]
fn mutex_rwlock_interaction() {
    let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
    let observed_values_clone = Arc::clone(&observed_values);

    check_dfs(
        move || {
            let lock = Arc::new(Mutex::new(()));
            let rwlock = Arc::new(shuttle::sync::RwLock::new(()));
            let value = Arc::new(std::sync::Mutex::new(0usize));

            let add_thread = {
                let lock = Arc::clone(&lock);
                let value = Arc::clone(&value);
                thread::spawn(move || {
                    {
                        let _guard = lock.lock().unwrap();
                        *value.lock().unwrap() += 1;
                    }
                    {
                        let _guard = rwlock.write().unwrap();
                        if let Ok(_g) = lock.try_lock() {
                            // In this case the multiplication in the other thread can go before,
                            // after, or between +1/+2/+3, resulting in final values 6, 7, 9, and 12.
                            *value.lock().unwrap() += 2;
                        } else {
                            // In this case the multiplication in the other thread can only go between
                            // +1 and +6 or between +6 and +3, resulting in final values 11 and 17.
                            // The multiplication cannot go first or last, which would be final values
                            // 10 and 20.
                            *value.lock().unwrap() += 6;
                        }
                    }
                    {
                        let _guard = lock.lock().unwrap();
                        *value.lock().unwrap() += 3;
                    }
                })
            };
            let mul_thread = {
                let lock = Arc::clone(&lock);
                let log = Arc::clone(&value);
                thread::spawn(move || {
                    let _guard = lock.lock().unwrap();
                    *log.lock().unwrap() *= 2;
                })
            };

            add_thread.join().unwrap();
            mul_thread.join().unwrap();

            let value = Arc::try_unwrap(value).unwrap().into_inner().unwrap();
            observed_values_clone.lock().unwrap().insert(value);
        },
        None,
    );

    let observed_values = Arc::try_unwrap(observed_values).unwrap().into_inner().unwrap();
    assert_eq!(observed_values, HashSet::from([6, 7, 9, 12, 11, 17]));
}

/// Two concurrent threads trying to do an atomic increment using `try_lock`.
/// One `try_lock` must succeed, while the other may or may not succeed.
/// Thus we expect to see final values 1 and 2.
#[test]
fn concurrent_try_increment() {
    let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
    let observed_values_clone = Arc::clone(&observed_values);

    check_dfs(
        move || {
            let lock = Arc::new(Mutex::new(0usize));

            let threads = (0..2)
                .map(|_| {
                    let lock = Arc::clone(&lock);
                    thread::spawn(move || {
                        match lock.try_lock() {
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

/// Two concurrent threads, one doing an atomic increment by 1 using `lock`,
/// the other trying to do an atomic increment by 1 followed by trying to do
/// an atomic increment by 2 using `try_lock`. The `lock` must succeed, while
/// both `try_lock`s may or may not succeed. Thus we expect to see final values
/// * 1 (both `try_lock`s fail),
/// * 2 (second `try_lock` fails),
/// * 3 (first `try_lock` fails), and
/// * 4 (both `try_lock`s succeed).
#[test]
fn concurrent_lock_try_lock() {
    let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
    let observed_values_clone = Arc::clone(&observed_values);

    check_dfs(
        move || {
            let lock = Arc::new(Mutex::new(0usize));

            let lock_thread = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    *lock.lock().unwrap() += 1;
                })
            };
            let try_lock_thread = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    for n in 1..3 {
                        match lock.try_lock() {
                            Ok(mut guard) => {
                                *guard += n;
                            }
                            Err(TryLockError::WouldBlock) => (),
                            Err(_) => panic!("unexpected TryLockError"),
                        };
                    }
                })
            };

            lock_thread.join().unwrap();
            try_lock_thread.join().unwrap();

            let value = Arc::try_unwrap(lock).unwrap().into_inner().unwrap();
            observed_values_clone.lock().unwrap().insert(value);
        },
        None,
    );

    let observed_values = Arc::try_unwrap(observed_values).unwrap().into_inner().unwrap();
    assert_eq!(observed_values, HashSet::from([1, 2, 3, 4]));
}

#[test]
#[should_panic(expected = "tried to acquire a Mutex it already holds")]
fn double_lock() {
    check_dfs(
        || {
            let mutex = Mutex::new(());
            let _guard_1 = mutex.lock().unwrap();
            let _guard_2 = mutex.lock();
        },
        None,
    )
}

#[test]
fn double_try_lock() {
    check_dfs(
        || {
            let mutex = Mutex::new(());
            let _guard_1 = mutex.try_lock().unwrap();
            assert!(matches!(mutex.try_lock(), Err(TryLockError::WouldBlock)));
        },
        None,
    )
}

// Check that we can safely execute the Drop handler of a Mutex without double-panicking
#[test]
#[should_panic(expected = "expected panic")]
fn panic_drop() {
    check_dfs(
        || {
            let lock = Mutex::new(0);
            let _l = lock.lock().unwrap();
            panic!("expected panic");
        },
        None,
    )
}

#[test]
fn mutex_into_inner() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(0u64));

            let threads = (0..2)
                .map(|_| {
                    let lock = lock.clone();
                    thread::spawn(move || {
                        *lock.lock().unwrap() += 1;
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

/// Checks that the mutex is unfair, i.e. if there are two threads blocked
/// on the same mutex, unlocking the mutex may unblock either thread.
#[test]
fn mutex_unfair() {
    let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
    let observed_values_clone = Arc::clone(&observed_values);

    check_random(
        move || {
            let lock = Arc::new(Mutex::new(vec![]));
            let guard = lock.lock().unwrap();

            // Here we use a stdlib mutex to avoid introducing yield points.
            // It is used to record in which order the two threads were
            // enqueued, because `thread::spawn` is a yield point and as such
            // the enqueing can happen in either order.
            let order = Arc::new(std::sync::Mutex::new(vec![]));
            let threads = (0..2)
                .map(|tid| {
                    let lock = lock.clone();
                    let order = order.clone();
                    thread::spawn(move || {
                        // once the ID is pushed to the vector here and
                        // observed in the busy loop below, the thread is
                        // assumed to be blocked, because there is no yield
                        // point between the push and the shuttle mutex lock
                        order.lock().unwrap().push(tid); // stdlib mutex
                        lock.lock().unwrap().push(tid); // shuttle mutex
                    })
                })
                .collect::<Vec<_>>();

            while order.lock().unwrap().len() < 2 {
                thread::yield_now();
            }
            drop(guard);

            for thread in threads {
                thread.join().unwrap();
            }

            let lock = Arc::try_unwrap(lock).unwrap();
            let order = Arc::try_unwrap(order).unwrap();
            observed_values_clone
                .lock()
                .unwrap()
                .insert((lock.into_inner().unwrap(), order.into_inner().unwrap()));
        },
        100, // 100 tries should be enough to find both orders
    );

    // Once we are done, we should see four combinations: the threads can wait
    // on the mutex in either order, and once the mutex is available, the
    // threads can be unblock in either order as well.
    let observed_values = Arc::try_unwrap(observed_values).unwrap().into_inner().unwrap();
    assert_eq!(
        observed_values,
        HashSet::from([
            (vec![0, 1], vec![0, 1]),
            (vec![1, 0], vec![0, 1]),
            (vec![0, 1], vec![1, 0]),
            (vec![1, 0], vec![1, 0]),
        ])
    );
}
