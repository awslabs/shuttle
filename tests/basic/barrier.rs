use shuttle::sync::{mpsc::channel, Barrier};
use shuttle::{check_dfs, thread};
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use test_log::test;

#[test]
fn barrier_simple() {
    // Run two threads, one incrementing and the other decrementing a shared counter,
    // with each one pausing at the barrier after a single operation.
    // Check the invariant that the counter is always within +/- 1 of the starting value.
    check_dfs(
        || {
            let barrier = Arc::new(Barrier::new(2));
            let counter = Arc::new(AtomicUsize::new(100));

            let barrier2 = Arc::clone(&barrier);
            let counter2 = Arc::clone(&counter);

            // incrementer
            thread::spawn(move || {
                for _ in 0..5 {
                    let old = counter2.fetch_add(1, Ordering::SeqCst);
                    assert!((99..=100).contains(&old));
                    barrier2.wait();
                }
            });

            // decrementer
            thread::spawn(move || {
                for _ in 0..5 {
                    let old = counter.fetch_sub(1, Ordering::SeqCst);
                    assert!((100..=101).contains(&old));
                    barrier.wait();
                }
            });
        },
        None,
    );
}

// Slightly generalized version of barrier test from std::sync
fn barrier_test(n: usize, c: usize) {
    let barrier = Arc::new(Barrier::new(c));
    let (tx, rx) = channel();

    let handles = (0..n - 1)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let tx = tx.clone();
            thread::spawn(move || {
                tx.send(barrier.wait().is_leader()).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // At this point, all spawned threads should be blocked,
    // so we shouldn't get anything from the port
    // TODO uncomment this line after we add support for try_recv()
    // assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));

    let mut leader_found = barrier.wait().is_leader();

    // Now, the barrier is cleared and we should get data.
    for _ in 0..n - 1 {
        if rx.recv().unwrap() {
            assert!(!leader_found);
            leader_found = true;
        }
    }
    assert!(leader_found);

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn barrier_test_many_ok() {
    check_dfs(|| barrier_test(4, 4), None);
}

#[test]
#[should_panic(expected = "deadlock")]
fn barrier_test_many_block() {
    check_dfs(|| barrier_test(3, 4), None);
}

#[test]
fn barrier_test_one_thread() {
    check_dfs(|| barrier_test(1, 1), None);
}

#[test]
fn barrier_test_reuse_one() {
    check_dfs(
        || {
            let barrier = Barrier::new(1);
            for _ in 0..5 {
                assert!(barrier.wait().is_leader());
            }
        },
        None,
    );
}

#[test]
fn barrier_test_reuse_zero() {
    check_dfs(
        || {
            let barrier = Barrier::new(0);
            for _ in 0..5 {
                assert!(barrier.wait().is_leader());
            }
        },
        None,
    );
}

// Test that reusing a barrier on a new thread results in the new thread becoming the leader.
#[test]
fn barrier_test_reuse_different_thread() {
    check_dfs(
        || {
            let barrier = Barrier::new(1);
            assert!(barrier.wait().is_leader());

            let thd = thread::spawn(move || {
                assert!(barrier.wait().is_leader());
            });

            thd.join().unwrap();
        },
        None,
    );
}

// Test that any combination of threads can be chosen as leader of an epoch.
fn barrier_test_nondeterministic_leader(batch_size: usize, num_threads: usize) {
    // Don't test cases that might deadlock; every thread needs to be released eventually
    assert_eq!(num_threads % batch_size, 0);

    let seen_leaders = Arc::new(std::sync::Mutex::new(HashSet::new()));
    {
        let seen_leaders = Arc::clone(&seen_leaders);
        check_dfs(
            move || {
                let barrier = Arc::new(Barrier::new(batch_size));
                let leaders = Arc::new(std::sync::Mutex::new(vec![]));
                let handles = (0..num_threads)
                    .map(|i| {
                        let barrier = Arc::clone(&barrier);
                        let leaders = Arc::clone(&leaders);

                        thread::spawn(move || {
                            let result = barrier.wait();
                            if result.is_leader() {
                                leaders.lock().unwrap().push(i);
                            }
                        })
                    })
                    .collect::<Vec<_>>();

                for thd in handles {
                    thd.join().unwrap();
                }

                let leaders = Arc::try_unwrap(leaders).unwrap().into_inner().unwrap();
                seen_leaders.lock().unwrap().insert(leaders);
            },
            None,
        );
    }

    let seen_leaders = Arc::try_unwrap(seen_leaders).unwrap().into_inner().unwrap();
    let num_batches = num_threads / batch_size;
    // Number of ways to unique order `r` distinct elements from list of `n` total elements.
    let perm = |n: usize, r: usize| (n - r + 1..=n).reduce(|acc, i| acc * i).unwrap_or(1);

    // Every execution should have one leader per batch
    assert!(seen_leaders.iter().all(|l| l.len() == num_batches));
    // There should be `num_threads permute num_batches` different leader vectors
    assert_eq!(seen_leaders.len(), perm(num_threads, num_batches));
}

#[test]
fn barrier_test_nondeterministic_leader_1() {
    barrier_test_nondeterministic_leader(1, 4);
}

#[test]
fn barrier_test_nondeterministic_leader_2() {
    barrier_test_nondeterministic_leader(2, 4);
}

#[test]
fn barrier_test_nondeterministic_leader_4() {
    barrier_test_nondeterministic_leader(4, 4);
}
