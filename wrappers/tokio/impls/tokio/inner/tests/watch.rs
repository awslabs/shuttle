use shuttle::{check_dfs, check_random, current, future};
use shuttle_tokio_impl_inner::sync::watch;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use test_log::test;

#[test]
fn watch_basic() {
    check_random(
        || {
            future::block_on(async {
                let (ctx1, crx) = shuttle::sync::mpsc::channel();
                let ctx2 = ctx1.clone();

                let (tx, mut rx1) = watch::channel(0usize);
                let mut rx2 = rx1.clone();

                let p = future::spawn(async move {
                    let _ = tx.send(1);
                    let v1 = crx.recv().unwrap();
                    let v2 = crx.recv().unwrap();
                    assert!(v1 == 1 && v2 == 1);

                    let _ = tx.send(2);
                    let v1 = crx.recv().unwrap();
                    let v2 = crx.recv().unwrap();
                    assert!(v1 == 2 && v2 == 2);
                });
                let h1 = future::spawn(async move {
                    while rx1.changed().await.is_ok() {
                        ctx1.send(*rx1.borrow()).unwrap();
                    }
                });
                let h2 = future::spawn(async move {
                    while rx2.changed().await.is_ok() {
                        ctx2.send(*rx2.borrow()).unwrap();
                    }
                });
                futures::future::join_all([p, h1, h2]).await;
            });
        },
        20_000,
    );
}

#[test]
fn watch_changed() {
    let values = Arc::new(Mutex::new(HashSet::new()));
    let values2 = values.clone();
    check_random(
        move || {
            let values = values2.clone();
            future::block_on(async {
                let (tx, mut rx) = watch::channel(0usize);
                let h = future::spawn(async move {
                    // Collect the set of values we see
                    // Note: we use a set since we may see the same value twice
                    let mut s = HashSet::new();
                    while rx.changed().await.is_ok() {
                        s.insert(*rx.borrow());
                    }
                    // Store the sum in shared stoate
                    let sum = s.iter().sum::<usize>();
                    values.lock().unwrap().insert(sum);
                });
                let _ = tx.send(1);
                let _ = tx.send(2);
                drop(tx);
                h.await.unwrap();
            });
        },
        // The `values` set below reaches its final contents within the first handful of iterations,
        // so 200_000 was three orders of magnitude more than the assertion needs.
        20_000,
    );

    let values = values.lock().unwrap();
    assert_eq!(*values, HashSet::from([2, 3]));
}

/*
/// Outstanding borrows hold a read lock on the inner value. This means that
/// long lived borrows could cause the produce half to block. It is recommended
/// to keep the borrow as short lived as possible.
/// <details><summary>Potential deadlock example</summary>
///
/// ```text
/// // Task 1 (on thread A)    |  // Task 2 (on thread B)
/// let _ref1 = rx.borrow();   |
///                            |  // will block
///                            |  let _ = tx.send(());
/// // may deadlock            |
/// let _ref2 = rx.borrow();   |
/// ```
///
///
///
 */

#[test]
fn wait_for_test() {
    check_random(
        move || {
            let (tx, mut rx) = watch::channel(false);

            let tx_arc = Arc::new(tx);
            let tx1 = tx_arc.clone();
            let tx2 = tx_arc.clone();

            let th1 = shuttle::thread::spawn(move || {
                for _ in 0..2 {
                    tx1.send_modify(|_x| {});
                }
            });

            let th2 = shuttle::thread::spawn(move || {
                tx2.send(true).unwrap();
            });

            assert!(*shuttle::future::block_on(rx.wait_for(|x| *x)).unwrap());

            th1.join().unwrap();
            th2.join().unwrap();
        },
        10_000,
    );
}

// Adapted from the Loom test in Tokio with the same name.
#[test]
fn wait_for_returns_correct_value() {
    check_dfs(
        move || {
            let (tx, mut rx) = watch::channel(0);

            let jh = shuttle::thread::spawn(move || {
                tx.send(1).unwrap();
                tx.send(2).unwrap();
                tx.send(3).unwrap();
            });

            // Stop at the first value we are called at.
            let mut stopped_at = None;
            let returned = *shuttle::future::block_on(rx.wait_for(|x| {
                stopped_at = Some(*x);
                true
            }))
            .unwrap();

            // Check that it returned the same value as the one we returned
            // `true` for.
            assert_eq!(stopped_at.unwrap(), returned);

            jh.join().unwrap();
        },
        None,
    );
}

/// Regression test: Shuttle must be able to schedule another task between a sender's previous
/// operation and its `send()`.
///
/// The test program wrongly assumes that the receiver can't be dropped between the sender's store
/// and its `send()`. That fails when the receiver's task runs between the two, which Shuttle only
/// explores if `send()` takes a scheduling point before it reads the receiver count.
#[test]
#[should_panic(expected = "closed between the store and send()")]
fn send_after_store_is_a_separate_step() {
    check_dfs(
        || {
            let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
            let (tx, rx) = watch::channel(0);
            let receiver = {
                let ready = ready.clone();
                shuttle::thread::spawn(move || if ready.load(Ordering::SeqCst) { None } else { Some(rx) })
            };

            ready.store(true, Ordering::SeqCst);
            // BUG (in this test program): assumes the receiver can't be dropped before `send()`.
            assert!(tx.send(1).is_ok(), "closed between the store and send()");

            drop(receiver.join().unwrap());
        },
        None,
    );
}

/// Regression test: Shuttle must be able to schedule another task between a sender's previous
/// operation and its `subscribe()`, which can reopen a closed channel.
///
/// The test program wrongly assumes that once it sees `ready`, the new receiver exists, so that a
/// `send()` succeeds. That fails when the other sender runs between the store and `subscribe()`,
/// which Shuttle only explores if `subscribe()` takes a scheduling point first.
#[test]
#[should_panic(expected = "subscribe() not done yet")]
fn subscribe_after_store_is_a_separate_step() {
    check_dfs(
        || {
            let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
            let (tx, rx) = watch::channel(0);
            drop(rx);
            let other = {
                let (tx, ready) = (tx.clone(), ready.clone());
                shuttle::thread::spawn(move || {
                    if ready.load(Ordering::SeqCst) {
                        // BUG (in this test program): assumes `subscribe()` has already run.
                        assert!(tx.send(1).is_ok(), "subscribe() not done yet");
                    }
                })
            };

            ready.store(true, Ordering::SeqCst);
            let _rx = tx.subscribe();

            other.join().unwrap();
        },
        None,
    );
}

/// Regression test: Shuttle must be able to schedule another task between a sender's previous
/// operation and the drop of the last `Sender`, which closes the channel.
///
/// The test program wrongly assumes that once it sees `ready`, the channel is closed. That fails
/// when the receiver runs between the store and the drop, which Shuttle only explores if the drop
/// takes a scheduling point first.
#[test]
#[should_panic(expected = "sender not dropped yet")]
fn last_sender_drop_after_store_is_a_separate_step() {
    check_dfs(
        || {
            let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
            let (tx, rx) = watch::channel(0);
            let receiver = {
                let ready = ready.clone();
                shuttle::thread::spawn(move || {
                    if ready.load(Ordering::SeqCst) {
                        // BUG (in this test program): assumes the sender has already been dropped.
                        assert!(rx.has_changed().is_err(), "sender not dropped yet");
                    }
                })
            };

            ready.store(true, Ordering::SeqCst);
            drop(tx);

            receiver.join().unwrap();
        },
        None,
    );
}

/// Regression test: Shuttle must be able to schedule another task between a receiver's previous
/// operation and the drop of the last `Receiver`, which closes the channel.
///
/// The test program wrongly assumes that once it sees `ready`, the channel is closed, so that
/// `send()` fails. That fails when the sender runs between the store and the drop, which Shuttle
/// only explores if the drop takes a scheduling point first.
#[test]
#[should_panic(expected = "receiver not dropped yet")]
fn last_receiver_drop_after_store_is_a_separate_step() {
    check_dfs(
        || {
            let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
            let (tx, rx) = watch::channel(0);
            let sender = {
                let ready = ready.clone();
                shuttle::thread::spawn(move || {
                    if ready.load(Ordering::SeqCst) {
                        // BUG (in this test program): assumes the receiver has already been dropped.
                        assert!(tx.send(1).is_err(), "receiver not dropped yet");
                    }
                })
            };

            ready.store(true, Ordering::SeqCst);
            drop(rx);

            sender.join().unwrap();
        },
        None,
    );
}

/// Returns the output of `f` and the number of scheduling points that it took.
async fn with_scheduling_points<F: std::future::Future>(f: F) -> (F::Output, usize) {
    let before = current::context_switches();
    let output = f.await;
    (output, current::context_switches() - before)
}

/// A wait that ends at once takes one scheduling point, before it checks the state, and so does
/// the drop of the last `Sender` or `Receiver`. Waking the waiters takes none of its own.
#[test]
fn waits_and_last_drops_take_one_scheduling_point() {
    check_dfs(
        || {
            future::block_on(async {
                let (tx, mut rx) = watch::channel(0);
                tx.send(1).unwrap();
                let (result, points) = with_scheduling_points(rx.changed()).await;
                assert!(result.is_ok());
                assert_eq!(points, 1);

                let ((), points) = with_scheduling_points(async { drop(tx) }).await;
                assert_eq!(points, 1);
                let (result, points) = with_scheduling_points(rx.changed()).await;
                assert!(result.is_err());
                assert_eq!(points, 1);

                let (tx, rx) = watch::channel(0);
                let ((), points) = with_scheduling_points(async { drop(rx) }).await;
                assert_eq!(points, 1);
                let ((), points) = with_scheduling_points(tx.closed()).await;
                assert_eq!(points, 1);
            });
        },
        None,
    );
}

/// A `Sender` and a `Receiver` that outlive their execution can be dropped. The last drop of each
/// takes no scheduling point where there is no execution to switch in, and waking the waiters
/// takes none at all. The channel is made inside an execution, kept in a `static` past the end of
/// `check_dfs`, and dropped by the test thread.
#[test]
fn last_drops_outside_an_execution() {
    static CHANNELS: Mutex<Vec<(watch::Sender<usize>, watch::Receiver<usize>)>> = Mutex::new(Vec::new());

    check_dfs(
        || {
            CHANNELS.lock().unwrap().push(watch::channel(0));
        },
        None,
    );

    let mut channels = CHANNELS.lock().unwrap();
    assert_eq!(channels.len(), 1, "the execution should have made one channel");
    channels.clear();
}
