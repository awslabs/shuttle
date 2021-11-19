use rand::Rng;
use shuttle::sync::{Condvar, Mutex};
use shuttle::{check_dfs, check_random, replay, thread};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use test_env_log::test;

#[test]
fn notify_one() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(false));
            let cond = Arc::new(Condvar::new());

            {
                let lock = Arc::clone(&lock);
                let cond = Arc::clone(&cond);
                thread::spawn(move || {
                    let mut guard = lock.lock().unwrap();
                    while !*guard {
                        guard = cond.wait(guard).unwrap();
                    }
                });
            }

            *lock.lock().unwrap() = true;
            // Note: it's valid to signal a condvar while not holding the corresponding lock
            cond.notify_one();
        },
        None,
    )
}

#[test]
fn notify_one_while() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(false));
            let cond = Arc::new(Condvar::new());

            {
                let lock = Arc::clone(&lock);
                let cond = Arc::clone(&cond);
                thread::spawn(move || {
                    let guard = lock.lock().unwrap();
                    let guard = cond.wait_while(guard, |flag| !*flag).unwrap();
                    assert!(*guard);
                });
            }

            *lock.lock().unwrap() = true;
            // Note: it's valid to signal a condvar while not holding the corresponding lock
            cond.notify_one();
        },
        None,
    )
}

fn two_workers<F>(signal_thread: F)
where
    F: Fn(Arc<Condvar>),
{
    let lock = Arc::new(Mutex::new(false));
    let cond = Arc::new(Condvar::new());

    for _ in 0..2 {
        let lock = Arc::clone(&lock);
        let cond = Arc::clone(&cond);
        thread::spawn(move || {
            let mut guard = lock.lock().unwrap();
            while !*guard {
                guard = cond.wait(guard).unwrap();
            }
        });
    }

    *lock.lock().unwrap() = true;
    signal_thread(cond);
}

#[test]
fn notify_all() {
    check_dfs(|| two_workers(|cond| cond.notify_all()), None)
}

#[test]
fn multiple_notify_one() {
    check_dfs(
        || {
            two_workers(|cond| {
                cond.notify_one();
                cond.notify_one();
            })
        },
        None,
    )
}

#[test]
#[should_panic(expected = "deadlock")]
fn notify_one_deadlock() {
    check_dfs(
        || {
            two_workers(|cond| {
                cond.notify_one();
                // only one signal, so there exists an execution where the second worker is never woken
            })
        },
        None,
    )
}

#[test]
fn notify_one_all() {
    check_dfs(
        || {
            two_workers(|cond| {
                cond.notify_one();
                cond.notify_all();
            })
        },
        None,
    )
}

#[test]
fn notify_all_one() {
    check_dfs(
        || {
            two_workers(|cond| {
                cond.notify_all();
                cond.notify_one();
            })
        },
        None,
    )
}

#[test]
#[should_panic(expected = "found the failing execution")]
fn notify_one_order() {
    check_dfs(
        || {
            // All the complexity in this test is to arrange a specific order of threads arriving in the
            // waiters queue for `cond`. We arrange for Thread 1 to always be the first thread to wait
            // on `cond`, but for both threads to be waiting on `cond` before we call `cond.notify_one`.
            // Therefore, either thread should be eligible to wake up, and if Thread 2 wakes up, it can
            // cause the assertion in this test to fail.
            //
            // This test does not fail in Loom, because its Condvar impl always chooses the first waiter
            // to unblock, which is not a guarantee Condvar provides.

            // The actual lock and condvar we care about
            let lock = Arc::new(Mutex::new(0u8));
            let cond = Arc::new(Condvar::new());

            // Auxiliary cond used to sequence the threads in the desired way
            let sequencer_cond = Arc::new(Condvar::new());

            // Thread 1
            {
                let lock = Arc::clone(&lock);
                let cond = Arc::clone(&cond);
                let sequencer_cond = Arc::clone(&sequencer_cond);
                thread::spawn(move || {
                    let mut guard = lock.lock().unwrap();
                    while *guard != 1 {
                        guard = sequencer_cond.wait(guard).unwrap();
                    }
                    *guard = 2;
                    sequencer_cond.notify_all();

                    while *guard < 5 {
                        guard = cond.wait(guard).unwrap();
                    }
                    *guard = 10;
                });
            }

            // Thread 2
            {
                let lock = Arc::clone(&lock);
                let cond = Arc::clone(&cond);
                let sequencer_cond = Arc::clone(&sequencer_cond);
                thread::spawn(move || {
                    let mut guard = lock.lock().unwrap();
                    while *guard != 3 {
                        guard = sequencer_cond.wait(guard).unwrap();
                    }
                    *guard = 4;
                    sequencer_cond.notify_all();

                    while *guard < 5 {
                        guard = cond.wait(guard).unwrap();
                    }
                    *guard = 20;
                });
            }

            // Thread 0
            let mut guard = lock.lock().unwrap();
            *guard = 1;
            sequencer_cond.notify_all();
            while *guard != 2 {
                guard = sequencer_cond.wait(guard).unwrap();
            }
            *guard = 3;
            sequencer_cond.notify_all();
            while *guard != 4 {
                guard = sequencer_cond.wait(guard).unwrap();
            }
            *guard = 5;

            // At this point we are guaranteed that both Thread 1 and Thread 2 are waiting on `cond`,
            // and that Thread 1 was the first thread to enter the waiter queue. If we haven't
            // implemented `notify_one` correctly, it might always wake Thread 1.
            cond.notify_one();

            drop(guard);

            // Check whether Thread 2 was woken
            assert_ne!(*lock.lock().unwrap(), 20, "found the failing execution");

            // Not necessary for the test; just prevent deadlock
            cond.notify_one();
        },
        None,
    )
}

/// From "Operating Systems: Three Easy Pieces", Figure 30.8.
/// Demonstrates why a waiter needs to check the condition in a `while` loop, not an if.
/// http://pages.cs.wisc.edu/~remzi/OSTEP/threads-cv.pdf
#[test]
#[should_panic(expected = "nothing to get")]
fn producer_consumer_broken1() {
    replay(
        || {
            let lock = Arc::new(Mutex::new(()));
            let cond = Arc::new(Condvar::new());
            let count = Arc::new(AtomicUsize::new(0));

            // Two consumers
            for _ in 0..2 {
                let lock = Arc::clone(&lock);
                let cond = Arc::clone(&cond);
                let count = Arc::clone(&count);
                thread::spawn(move || {
                    for _ in 0..2 {
                        let mut guard = lock.lock().unwrap();
                        if count.load(Ordering::SeqCst) == 0 {
                            guard = cond.wait(guard).unwrap();
                        }
                        // get()
                        assert_eq!(count.load(Ordering::SeqCst), 1, "nothing to get");
                        count.store(0, Ordering::SeqCst);
                        cond.notify_one();
                        drop(guard); // explicit unlock to match the book
                    }
                });
            }

            // One producer
            for _ in 0..2 {
                let mut guard = lock.lock().unwrap();
                if count.load(Ordering::SeqCst) == 1 {
                    guard = cond.wait(guard).unwrap();
                }
                // put()
                assert_eq!(count.load(Ordering::SeqCst), 0, "no space to put");
                count.store(1, Ordering::SeqCst);
                cond.notify_one();
                drop(guard);
            }
        },
        "910215000000408224002229",
    )
}

/// From "Operating Systems: Three Easy Pieces", Figure 30.10. Like `producer_consumer_broken1`, but
/// with a while loop, not an if.
/// Demonstrates why one condvar is not sufficient for a concurrent queue.
/// http://pages.cs.wisc.edu/~remzi/OSTEP/threads-cv.pdf
#[test]
#[should_panic(expected = "deadlock")]
fn producer_consumer_broken2() {
    replay(
        || {
            let lock = Arc::new(Mutex::new(()));
            let cond = Arc::new(Condvar::new());
            let count = Arc::new(AtomicUsize::new(0));

            // Two consumers
            for _ in 0..2 {
                let lock = Arc::clone(&lock);
                let cond = Arc::clone(&cond);
                let count = Arc::clone(&count);
                thread::spawn(move || {
                    for _ in 0..1 {
                        let mut guard = lock.lock().unwrap();
                        while count.load(Ordering::SeqCst) == 0 {
                            guard = cond.wait(guard).unwrap();
                        }
                        // get()
                        assert_eq!(count.load(Ordering::SeqCst), 1, "nothing to get");
                        count.store(0, Ordering::SeqCst);
                        cond.notify_one();
                        drop(guard);
                    }
                });
            }

            // One producer
            for _ in 0..2 {
                let mut guard = lock.lock().unwrap();
                while count.load(Ordering::SeqCst) == 1 {
                    guard = cond.wait(guard).unwrap();
                }
                // put()
                assert_eq!(count.load(Ordering::SeqCst), 0, "no space to put");
                count.store(1, Ordering::SeqCst);
                cond.notify_one();
                drop(guard);
            }
        },
        "9102110090401004405202",
    )
}

/// From "Operating Systems: Three Easy Pieces", Figure 30.12. Like `producer_consumer_broken2`, but
/// uses separate condvars for "empty" and "full".
/// http://pages.cs.wisc.edu/~remzi/OSTEP/threads-cv.pdf
#[test]
fn producer_consumer_correct() {
    // Has been tested with check_dfs, but that's really slow
    check_random(
        || {
            let lock = Arc::new(Mutex::new(()));
            let is_empty = Arc::new(Condvar::new()); // count == 0
            let is_full = Arc::new(Condvar::new()); // count == 1
            let count = Arc::new(AtomicUsize::new(0));

            // Two consumers
            for _ in 0..2 {
                let lock = Arc::clone(&lock);
                let is_empty = Arc::clone(&is_empty);
                let is_full = Arc::clone(&is_full);
                let count = Arc::clone(&count);
                thread::spawn(move || {
                    for _ in 0..1 {
                        let mut guard = lock.lock().unwrap();
                        while count.load(Ordering::SeqCst) == 0 {
                            guard = is_full.wait(guard).unwrap();
                        }
                        // get()
                        assert_eq!(count.load(Ordering::SeqCst), 1, "nothing to get");
                        count.store(0, Ordering::SeqCst);
                        is_empty.notify_one();
                        drop(guard);
                    }
                });
            }

            // One producer
            for _ in 0..2 {
                let mut guard = lock.lock().unwrap();
                while count.load(Ordering::SeqCst) == 1 {
                    guard = is_empty.wait(guard).unwrap();
                }
                // put()
                assert_eq!(count.load(Ordering::SeqCst), 0, "no space to put");
                count.store(1, Ordering::SeqCst);
                is_full.notify_one();
                drop(guard);
            }
        },
        20000,
    )
}

#[test]
fn producer_consumer_random() {
    check_random(
        move || {
            let mut rng = shuttle::rand::thread_rng();

            let num_producers = 1 + rng.gen::<usize>() % 3;
            let num_consumers = 1 + rng.gen::<usize>() % 3;
            // make events divisible evenly across both the producers and consumers
            let num_events = (num_producers * num_consumers) * (1 + rng.gen::<usize>() % 4);

            let lock = Arc::new(Mutex::new(()));
            let is_empty = Arc::new(Condvar::new()); // count == 0
            let is_full = Arc::new(Condvar::new()); // count == 1
            let count = Arc::new(AtomicUsize::new(0));

            let consumers = (0..num_consumers)
                .map(|_| {
                    let lock = Arc::clone(&lock);
                    let is_empty = Arc::clone(&is_empty);
                    let is_full = Arc::clone(&is_full);
                    let count = Arc::clone(&count);
                    thread::spawn(move || {
                        let events = num_events / num_consumers;
                        for _ in 0..events {
                            let mut guard = lock.lock().unwrap();
                            while count.load(Ordering::SeqCst) == 0 {
                                guard = is_full.wait(guard).unwrap();
                            }
                            // get()
                            assert_eq!(count.load(Ordering::SeqCst), 1, "nothing to get");
                            count.store(0, Ordering::SeqCst);
                            is_empty.notify_one();
                            drop(guard);
                        }
                    })
                })
                .collect::<Vec<_>>();

            let producers = (0..num_producers)
                .map(|_| {
                    let lock = Arc::clone(&lock);
                    let is_empty = Arc::clone(&is_empty);
                    let is_full = Arc::clone(&is_full);
                    let count = Arc::clone(&count);
                    thread::spawn(move || {
                        let events = num_events / num_producers;
                        for _ in 0..events {
                            let mut guard = lock.lock().unwrap();
                            while count.load(Ordering::SeqCst) == 1 {
                                guard = is_empty.wait(guard).unwrap();
                            }
                            // put()
                            assert_eq!(count.load(Ordering::SeqCst), 0, "no space to put");
                            count.store(1, Ordering::SeqCst);
                            is_full.notify_one();
                            drop(guard);
                        }
                    })
                })
                .collect::<Vec<_>>();

            for consumer in consumers {
                consumer.join().unwrap();
            }
            for producer in producers {
                producer.join().unwrap();
            }
        },
        5000,
    )
}

#[test]
fn notify_one_timeout() {
    // TODO we don't currently implement timeouts in `wait_timeout`, so this test is identical
    // TODO to `notify_one`.
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(false));
            let cond = Arc::new(Condvar::new());

            {
                let lock = Arc::clone(&lock);
                let cond = Arc::clone(&cond);
                thread::spawn(move || {
                    let mut guard = lock.lock().unwrap();
                    while !*guard {
                        guard = cond.wait_timeout(guard, Duration::from_secs(10)).unwrap().0;
                    }
                });
            }

            *lock.lock().unwrap() = true;
            // Note: it's valid to signal a condvar while not holding the corresponding lock
            cond.notify_one();
        },
        None,
    )
}

#[test]
fn notify_one_while_timeout() {
    // TODO we don't currently implement timeouts in `wait_timeout`, so this test is identical
    // TODO to `notify_one_while`.
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(false));
            let cond = Arc::new(Condvar::new());

            {
                let lock = Arc::clone(&lock);
                let cond = Arc::clone(&cond);
                thread::spawn(move || {
                    let guard = lock.lock().unwrap();
                    let (guard, timeout) = cond
                        .wait_timeout_while(guard, Duration::from_secs(10), |flag| !*flag)
                        .unwrap();
                    assert!(*guard);
                    assert!(!timeout.timed_out());
                });
            }

            *lock.lock().unwrap() = true;
            // Note: it's valid to signal a condvar while not holding the corresponding lock
            cond.notify_one();
        },
        None,
    )
}
