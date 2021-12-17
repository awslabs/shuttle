use shuttle::sync::{mpsc::channel, Barrier};
use shuttle::{check_dfs, thread};
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
