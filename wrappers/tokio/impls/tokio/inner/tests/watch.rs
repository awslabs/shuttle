use shuttle::{check_dfs, check_random, future};
use shuttle_tokio_impl_inner::sync::watch;
use std::collections::HashSet;
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
        200_000,
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
        200_000,
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
        50_000,
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
