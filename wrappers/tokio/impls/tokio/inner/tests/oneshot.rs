use shuttle::{check_dfs, check_random, future};
use shuttle_tokio_impl_inner::sync::{mpsc, oneshot};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[macro_export]
macro_rules! assert_pending {
    ($e:expr) => {{
        use core::task::Poll::*;
        match $e {
            Pending => {}
            Ready(v) => panic!("ready; value = {:?}", v),
        }
    }};
    ($e:expr, $($msg:tt)+) => {{
        use core::task::Poll::*;
        match $e {
            Pending => {}
            Ready(v) => {
                panic!("ready; value = {:?}; {}", v, format_args!($($msg)+))
            }
        }
    }};
}

#[test]
fn async_oneshot_send_recv() {
    check_dfs(
        || {
            future::block_on(async {
                let (tx, mut rx) = oneshot::channel();

                let h1 = future::spawn(async move {
                    let result = rx.try_recv();
                    if let Err(e) = result {
                        assert_eq!(e, oneshot::error::TryRecvError::Empty);
                        let result = rx.await;
                        assert_eq!(result, Ok(1));
                    } else {
                        assert_eq!(result, Ok(1));
                    }
                });

                let h2 = future::spawn(async move {
                    assert!(!tx.is_closed());
                    assert!(tx.send(1).is_ok());
                });

                futures::future::join_all(vec![h1, h2]).await;
            });
        },
        None,
    );
}

#[test]
fn async_oneshot_commands() {
    // Test a common use case for oneshot channels: as a way to acknowledge completion of an activity.
    // A client asks the server to do some work (increment a shared counter) and reply when done.
    check_dfs(
        || {
            future::block_on(async {
                let counter = Arc::new(AtomicUsize::new(0));
                let counter2 = counter.clone();
                let (tx, mut rx) = mpsc::channel(1);

                // Client
                let h1 = future::spawn(async move {
                    let (ctx, crx) = oneshot::channel();
                    tx.send(ctx).await.unwrap();
                    crx.await.unwrap();
                    assert_eq!(counter.load(Ordering::SeqCst), 3);
                });

                // Server
                let h2 = future::spawn(async move {
                    let ctx = rx.recv().await.unwrap();
                    for _ in 0..3 {
                        counter2.fetch_add(1, Ordering::SeqCst);
                    }
                    ctx.send(()).unwrap();
                });
                futures::future::join_all(vec![h1, h2]).await;
            });
        },
        None,
    );
}

#[test]
#[should_panic(expected = "assertion failed")]
fn async_oneshot_yield() {
    check_dfs(
        || {
            let (mut tx1, mut rx1) = oneshot::channel::<()>();
            let (tx2, mut rx2) = oneshot::channel::<()>();
            future::block_on(async {
                let h = future::spawn(async move {
                    rx1.close();
                    rx2.close();
                });
                tx1.closed().await; // wait for first channel to be closed
                assert!(tx2.is_closed()); // Some execution should fail this assertion
                h.await.unwrap();
            });
        },
        None,
    );
}

// If there is no synchronization point in `try_recv` (as was the case previously), then
// this test will hang forever.
#[test]
fn try_recv_loop() {
    check_random(
        || {
            let (tx1, mut rx1) = oneshot::channel::<()>();
            future::block_on(async {
                let try_recv_task = future::spawn(async move { while rx1.try_recv().is_err() {} });
                tx1.send(()).unwrap();
                try_recv_task.await.unwrap();
            });
        },
        100,
    );
}

/// Regression test: Shuttle must be able to schedule another task between a receiver's previous
/// operation and the first poll of its `Receiver`.
///
/// The test program wrongly assumes that the sender can't send between the receiver's store and
/// its first poll. That fails when the sender runs between the two, which Shuttle only explores if
/// the first poll takes a scheduling point.
#[test]
#[should_panic(expected = "sent between the store and the first poll")]
fn first_poll_after_store_is_a_separate_step() {
    check_dfs(
        || {
            let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
            let (tx, mut rx) = oneshot::channel();

            let sender = {
                let ready = ready.clone();
                shuttle::thread::spawn(move || {
                    if ready.load(Ordering::SeqCst) {
                        let _ = tx.send(1);
                    }
                })
            };

            ready.store(true, Ordering::SeqCst);
            let poll = future::block_on(async { futures::poll!(&mut rx) });
            // BUG (in this test program): assumes the sender can't run before the first poll.
            assert!(
                !matches!(poll, std::task::Poll::Ready(Ok(1))),
                "sent between the store and the first poll"
            );

            sender.join().unwrap();
        },
        None,
    );
}

/// Spawns a task that drops `rx` if it sees `ready`, and otherwise returns it, so that the
/// receiver isn't dropped until the test joins the task.
fn drop_receiver_if_ready(
    ready: &Arc<shuttle::sync::atomic::AtomicBool>,
    rx: oneshot::Receiver<()>,
) -> shuttle::thread::JoinHandle<Option<oneshot::Receiver<()>>> {
    let ready = ready.clone();
    shuttle::thread::spawn(move || if ready.load(Ordering::SeqCst) { None } else { Some(rx) })
}

/// Regression test: Shuttle must be able to schedule another task between a sender's previous
/// operation and the first poll of `Sender::closed()`.
///
/// The test program wrongly assumes that the receiver can't be dropped between the sender's store
/// and its first poll of `closed()`. That fails when the receiver's task runs between the two,
/// which Shuttle only explores if `closed()` takes a scheduling point before its first read.
#[test]
#[should_panic(expected = "closed between the store and closed()")]
fn closed_after_store_is_a_separate_step() {
    check_dfs(
        || {
            let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
            let (mut tx, rx) = oneshot::channel::<()>();
            let receiver = drop_receiver_if_ready(&ready, rx);

            ready.store(true, Ordering::SeqCst);
            let poll = future::block_on(async { futures::poll!(std::pin::pin!(tx.closed())) });
            // BUG (in this test program): assumes the receiver can't be dropped before `closed()`.
            assert!(poll.is_pending(), "closed between the store and closed()");

            drop(receiver.join().unwrap());
        },
        None,
    );
}

/// Regression test: as `closed_after_store_is_a_separate_step`, for `Sender::poll_closed()`.
#[test]
#[should_panic(expected = "closed between the store and poll_closed()")]
fn poll_closed_after_store_is_a_separate_step() {
    check_dfs(
        || {
            let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
            let (mut tx, rx) = oneshot::channel::<()>();
            let receiver = drop_receiver_if_ready(&ready, rx);

            ready.store(true, Ordering::SeqCst);
            let poll = future::block_on(async { futures::poll!(futures::future::poll_fn(|cx| tx.poll_closed(cx))) });
            // BUG (in this test program): assumes the receiver can't be dropped before
            // `poll_closed()`.
            assert!(poll.is_pending(), "closed between the store and poll_closed()");

            drop(receiver.join().unwrap());
        },
        None,
    );
}

/// Regression test: Shuttle must be able to schedule another task between a sender's previous
/// operation and the drop of a `Sender` that hasn't sent.
///
/// The test program wrongly assumes that once it sees `ready`, the sender has been dropped, so
/// that the channel is closed. That fails when the receiver runs between the sender's store and
/// the drop, which Shuttle only explores if the drop takes a scheduling point.
#[test]
#[should_panic(expected = "sender not dropped yet")]
fn sender_drop_after_store_is_a_separate_step() {
    check_dfs(
        || {
            let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
            let (tx, mut rx) = oneshot::channel::<()>();

            let receiver = {
                let ready = ready.clone();
                shuttle::thread::spawn(move || {
                    if ready.load(Ordering::SeqCst) {
                        // BUG (in this test program): assumes the sender has been dropped.
                        assert_eq!(
                            rx.try_recv(),
                            Err(oneshot::error::TryRecvError::Closed),
                            "sender not dropped yet"
                        );
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
/// operation and the drop of a `Receiver` that hasn't received.
///
/// The test program wrongly assumes that once it sees `ready`, the receiver has been dropped, so
/// that the channel is closed. That fails when the sender runs between the receiver's store and
/// the drop, which Shuttle only explores if the drop takes a scheduling point.
#[test]
#[should_panic(expected = "receiver not dropped yet")]
fn receiver_drop_after_store_is_a_separate_step() {
    check_dfs(
        || {
            let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
            let (tx, rx) = oneshot::channel::<()>();

            let sender = {
                let ready = ready.clone();
                shuttle::thread::spawn(move || {
                    if ready.load(Ordering::SeqCst) {
                        // BUG (in this test program): assumes the receiver has been dropped.
                        assert!(tx.is_closed(), "receiver not dropped yet");
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

/// Regression test: Shuttle must be able to schedule another task between a receiver's previous
/// operation and a poll of its `Receiver` that follows `try_recv()`. The poll before `try_recv()`
/// returned `Pending`, but `try_recv()` registers no waker, so the next poll doesn't follow a
/// wake-up.
///
/// The test program wrongly assumes that the sender can't send between the receiver's store and
/// its poll. That fails when the sender runs between the two, which Shuttle only explores if the
/// poll takes a scheduling point.
#[test]
#[should_panic(expected = "sent between the store and the poll")]
fn poll_after_try_recv_and_store_is_a_separate_step() {
    check_dfs(
        || {
            let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
            let (tx, mut rx) = oneshot::channel();

            let sender = {
                let ready = ready.clone();
                shuttle::thread::spawn(move || {
                    if ready.load(Ordering::SeqCst) {
                        let _ = tx.send(1);
                    }
                })
            };

            let first = future::block_on(async { futures::poll!(&mut rx) });
            if first.is_pending() && rx.try_recv() == Err(oneshot::error::TryRecvError::Empty) {
                ready.store(true, Ordering::SeqCst);
                let poll = future::block_on(async { futures::poll!(&mut rx) });
                // BUG (in this test program): assumes the sender can't run before the poll.
                assert!(
                    !matches!(poll, std::task::Poll::Ready(Ok(1))),
                    "sent between the store and the poll"
                );
            }

            sender.join().unwrap();
        },
        None,
    );
}

/// Regression test: Shuttle must be able to schedule another task between a sender's previous
/// operation and a second call of `Sender::poll_closed()`. The first call returned `Pending`, but
/// the second call doesn't necessarily follow a wake-up.
///
/// The test program wrongly assumes that the receiver can't be dropped between the sender's store
/// and its second `poll_closed()`. That fails when the receiver's task runs between the two, which
/// Shuttle only explores if every call of `poll_closed()` takes a scheduling point.
#[test]
#[should_panic(expected = "closed between the store and the second poll_closed()")]
fn second_poll_closed_after_store_is_a_separate_step() {
    check_dfs(
        || {
            let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
            let (mut tx, rx) = oneshot::channel::<()>();
            let receiver = drop_receiver_if_ready(&ready, rx);

            let mut poll_closed =
                || future::block_on(async { futures::poll!(futures::future::poll_fn(|cx| tx.poll_closed(cx))) });
            if poll_closed().is_pending() {
                ready.store(true, Ordering::SeqCst);
                // BUG (in this test program): assumes the receiver can't be dropped before the
                // second `poll_closed()`.
                assert!(
                    poll_closed().is_pending(),
                    "closed between the store and the second poll_closed()"
                );
            }

            drop(receiver.join().unwrap());
        },
        None,
    );
}

/// A `Sender` and a `Receiver` that are dropped after their execution ended don't panic. Outside
/// an execution, their drops take no scheduling point.
#[test]
fn sender_and_receiver_dropped_after_execution() {
    type Kept = (oneshot::Sender<()>, oneshot::Receiver<()>);
    static KEPT: std::sync::Mutex<Option<Kept>> = std::sync::Mutex::new(None);
    check_dfs(
        || {
            // Keep both halves past the end of the execution.
            *KEPT.lock().unwrap() = Some(oneshot::channel());
        },
        None,
    );
    let (tx, rx) = KEPT.lock().unwrap().take().unwrap();
    drop(tx);
    drop(rx);
}

/// A `Sender` that is dropped while its task unwinds a caught panic closes the channel, so that the
/// receiver doesn't wait forever.
#[test]
fn sender_dropped_during_caught_panic_closes_the_channel() {
    check_dfs(
        || {
            let (tx, rx) = oneshot::channel::<()>();
            let handle = shuttle::thread::spawn(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _tx = tx;
                    panic!("expected");
                }))
                .unwrap_err();
            });
            assert!(future::block_on(rx).is_err());
            handle.join().unwrap();
        },
        None,
    );
}

/// A `Receiver` that is dropped while its task unwinds a caught panic closes the channel.
#[test]
fn receiver_dropped_during_caught_panic_closes_the_channel() {
    check_dfs(
        || {
            let (mut tx, rx) = oneshot::channel::<()>();
            let handle = shuttle::thread::spawn(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _rx = rx;
                    panic!("expected");
                }))
                .unwrap_err();
            });
            future::block_on(tx.closed());
            assert!(tx.send(()).is_err());
            handle.join().unwrap();
        },
        None,
    );
}
