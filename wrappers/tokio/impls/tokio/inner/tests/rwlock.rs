use shuttle::current::{me, set_label_for_task};
use shuttle::{check_dfs, check_random, future};
use shuttle_tokio_impl_inner::sync::{mpsc, RwLock};
use shuttle_tokio_impl_inner::time::{clear_triggers, timeout, trigger_timeouts};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use test_log::test;
use tracing::trace;

#[test]
fn async_rwlock_reader_concurrency() {
    let saw_concurrent_reads = Arc::new(AtomicBool::new(false));
    {
        let saw_concurrent_reads = Arc::clone(&saw_concurrent_reads);
        check_random(
            move || {
                let saw_concurrent_reads2 = Arc::clone(&saw_concurrent_reads);
                future::block_on(async move {
                    let rwlock = Arc::new(RwLock::new(0usize));
                    let readers = Arc::new(AtomicUsize::new(0));

                    {
                        let rwlock = Arc::clone(&rwlock);
                        let readers = Arc::clone(&readers);

                        let saw_concurrent_reads3 = Arc::clone(&saw_concurrent_reads2);

                        future::spawn(async move {
                            let counter = rwlock.read().await;
                            assert_eq!(*counter, 0);

                            readers.fetch_add(1, Ordering::SeqCst);

                            future::yield_now().await;

                            if readers.load(Ordering::SeqCst) == 2 {
                                saw_concurrent_reads3.store(true, Ordering::SeqCst);
                            }

                            readers.fetch_sub(1, Ordering::SeqCst);
                        });
                    }

                    let counter = rwlock.read().await;
                    assert_eq!(*counter, 0);

                    readers.fetch_add(1, Ordering::SeqCst);

                    future::yield_now().await;

                    if readers.load(Ordering::SeqCst) == 2 {
                        saw_concurrent_reads2.store(true, Ordering::SeqCst);
                    }

                    readers.fetch_sub(1, Ordering::SeqCst);
                });
            },
            100,
        );
    }

    assert!(saw_concurrent_reads.load(Ordering::SeqCst));
}

async fn deadlock() {
    let lock1 = Arc::new(RwLock::new(0usize));
    let lock2 = Arc::new(RwLock::new(0usize));
    let lock1_clone = Arc::clone(&lock1);
    let lock2_clone = Arc::clone(&lock2);

    future::spawn(async move {
        let _l1 = lock1_clone.read().await;
        let _l2 = lock2_clone.read().await;
    });

    let _l2 = lock2.write().await;
    let _l1 = lock1.write().await;
}

#[test]
#[should_panic(expected = "deadlock")]
fn async_rwlock_deadlock() {
    check_dfs(
        || {
            future::block_on(async {
                deadlock().await;
            });
        },
        None,
    );
}

#[test]
fn async_rwlock_two_writers() {
    check_dfs(
        || {
            future::block_on(async move {
                let lock = Arc::new(RwLock::new(1));
                let lock2 = lock.clone();

                future::spawn(async move {
                    let mut w = lock.write_owned().await;
                    *w += 1;
                    let v = *w;
                    let r = w.downgrade();
                    assert_eq!(*r, v);
                });

                future::spawn(async move {
                    let mut w = lock2.write_owned().await;
                    *w += 1;
                });
            });
        },
        None,
    );
}

// Check that multiple readers are allowed to read at a time
// This test should never deadlock.
#[test]
fn async_rwlock_allows_multiple_readers() {
    check_dfs(
        || {
            future::block_on(async move {
                let lock1 = Arc::new(RwLock::new(1));
                let lock2 = lock1.clone();

                let (s1, mut r1) = mpsc::unbounded_channel::<usize>();
                let (s2, mut r2) = mpsc::unbounded_channel::<usize>();

                future::spawn(async move {
                    let w = lock1.read().await;
                    s1.send(*w).unwrap(); // Send value to other thread
                    let r = r2.recv().await; // Wait for value from other thread
                    assert_eq!(r, Some(1));
                });

                future::spawn(async move {
                    let w = lock2.read().await;
                    s2.send(*w).unwrap();
                    let r = r1.recv().await;
                    assert_eq!(r, Some(1));
                });
            });
        },
        None,
    );
}

/// This test exposed a deadlock bug in the following situation. A task is holding a read lock.
/// Another task wants to acquire a write lock and gets blocked. We cancel that task. Yet another
/// task gets stuck acquiring a read lock, although the lock should be available.
///
/// The underlying bug in `BatchSemaphore` was fixed in https://github.com/awslabs/shuttle/pull/167.
#[test]
fn canceling_blocked_write_deadlock_bug() {
    check_dfs(
        || {
            future::block_on(async move {
                #[derive(Debug, Clone)]
                struct TimeoutLabel;

                clear_triggers();

                let lock = Arc::new(RwLock::new(()));
                let lock1 = lock.clone();
                let lock2 = lock.clone();

                // Acquire a read lock so that acquiring a write lock below gets blocked.
                // This corresponds to a leaked lock in the production code that initially hit the bug.
                let _read_guard = lock.read().await;

                let w = future::spawn(async move {
                    set_label_for_task(me(), TimeoutLabel);
                    trace!("acquiring write lock");
                    let _write_result = timeout(Duration::from_secs(1), lock1.write()).await;
                    assert!(_write_result.is_err());
                    trace!("acquiring write lock timed out");
                });

                let t = future::spawn(async {
                    trace!("trigger timeout");
                    trigger_timeouts(move |labels| labels.get::<TimeoutLabel>().is_some());
                });

                let r = future::spawn(async move {
                    trace!("acquiring read lock");
                    // This is the lock acquire where we could get suck.
                    let _read_guard = lock2.read().await;
                    trace!("acquired read lock");
                });

                w.await.unwrap();
                t.await.unwrap();
                r.await.unwrap();
            });
        },
        None,
    );
}
