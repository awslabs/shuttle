use crate::mpsc::error::TryRecvError;
use assert_matches::assert_matches;
use shuttle::future;
use shuttle::sync::Arc;
use shuttle::{check_dfs, check_random};
use shuttle_tokio_impl_inner::sync::{mpsc, oneshot};
use std::sync::atomic::Ordering;
use test_log::test;
use tracing::trace;

#[test]
fn async_mpsc_send_recv_unbounded() {
    check_dfs(
        || {
            future::block_on(async {
                let (tx, mut rx) = mpsc::unbounded_channel();

                future::spawn(async move {
                    assert!(tx.send(1).is_ok());
                    assert!(tx.send(2).is_ok());
                });

                assert_eq!(Some(1), rx.recv().await);
                assert_eq!(Some(2), rx.recv().await);
                assert_eq!(None, rx.recv().await);
            });
        },
        None,
    );
}

#[test]
fn mpsc_unbounded_len() {
    check_dfs(
        || {
            future::block_on(async {
                let (tx, mut rx) = mpsc::unbounded_channel();

                assert_eq!(0, rx.len());

                assert!(tx.send(1).is_ok());
                assert_eq!(1, rx.len());
                assert!(tx.send(2).is_ok());
                assert_eq!(2, rx.len());

                assert_eq!(Some(1), rx.recv().await);
                assert_eq!(1, rx.len());
                assert_eq!(Some(2), rx.recv().await);
                assert_eq!(0, rx.len());
                drop(tx);
                assert_eq!(None, rx.recv().await);
                assert_eq!(0, rx.len());
            });
        },
        None,
    );
}

#[test]
fn async_mpsc_commutative_senders() {
    check_dfs(
        || {
            future::block_on(async {
                let (tx, mut rx) = mpsc::unbounded_channel();
                let tx2 = tx.clone();

                future::spawn(async move {
                    tx.send(5).unwrap();
                });
                future::spawn(async move {
                    tx2.send(6).unwrap();
                });
                let mut val = rx.recv().await.unwrap();
                val += rx.recv().await.unwrap();
                assert_eq!(val, 11);
            });
        },
        None,
    );
}

fn ignore_result<A>(_: A) {}

#[test]
#[should_panic(expected = "expected panic: sends can happen in any order")]
fn async_mpsc_loom_non_commutative_senders1() {
    check_dfs(
        || {
            future::block_on(async {
                let (s, mut r) = mpsc::unbounded_channel();
                let s2 = s.clone();
                future::spawn(async move {
                    ignore_result(s.send(5));
                });
                future::spawn(async move {
                    ignore_result(s2.send(6));
                });
                let val = r.recv().await;
                assert_eq!(val, Some(5), "expected panic: sends can happen in any order");
                ignore_result(r.recv().await);
            });
        },
        None,
    );
}

#[test]
#[should_panic(expected = "expected panic: sends can happen in any order")]
fn async_mpsc_loom_non_commutative_senders2() {
    check_dfs(
        || {
            future::block_on(async {
                let (s, mut r) = mpsc::unbounded_channel();
                let s2 = s.clone();
                future::spawn(async move {
                    ignore_result(s.send(5));
                });
                future::spawn(async move {
                    ignore_result(s2.send(6));
                });
                let val = r.recv().await;
                assert_eq!(val, Some(6), "expected panic: sends can happen in any order");
                ignore_result(r.recv().await);
            });
        },
        None,
    );
}

#[test]
fn async_mpsc_drop_sender_unbounded() {
    check_dfs(
        || {
            future::block_on(async {
                let (tx, mut rx) = mpsc::unbounded_channel::<i32>();
                future::spawn(async move {
                    drop(tx);
                });
                assert!(rx.recv().await.is_none());
            });
        },
        None,
    );
}

#[test]
fn async_mpsc_drop_receiver_unbounded() {
    check_dfs(
        || {
            let (tx, rx) = mpsc::unbounded_channel();
            drop(rx);
            assert!(tx.send(1).is_err());
        },
        None,
    );
}

#[test]
fn async_mpsc_buffering_behavior() {
    check_dfs(
        || {
            future::block_on(async move {
                let (send, mut recv) = mpsc::unbounded_channel();
                let handle = future::spawn(async move {
                    send.send(1u8).unwrap();
                    send.send(2).unwrap();
                    send.send(3).unwrap();
                    drop(send);
                });

                // wait for the thread to join so we ensure the sender is dropped
                handle.await.unwrap();

                // values sent before the sender disconnects are still available afterwards
                assert_eq!(Some(1), recv.recv().await);
                assert_eq!(Some(2), recv.recv().await);
                assert_eq!(Some(3), recv.recv().await);
                // but after the values are exhausted, recv() returns None
                assert!(recv.recv().await.is_none());
            });
        },
        None,
    );
}

#[test]
fn async_mpsc_bounded_sum() {
    check_dfs(
        || {
            future::block_on(async move {
                let (tx, mut rx) = mpsc::channel::<i32>(5);
                future::spawn(async move {
                    for _ in 0..3 {
                        tx.send(1).await.unwrap();
                    }
                });
                let handle = future::spawn(async move {
                    let mut sum = 0;
                    for _ in 0..3 {
                        trace!("... waiting for value");
                        sum += rx.recv().await.unwrap();
                    }
                    sum
                });
                let r = handle.await.unwrap();
                assert_eq!(r, 3);
            });
        },
        None,
    );
}

#[test]
fn mpsc_bounded_len() {
    check_dfs(
        || {
            future::block_on(async {
                let (tx, mut rx) = mpsc::channel(5);

                assert_eq!(0, rx.len());

                tx.send(1).await.unwrap();
                assert_eq!(1, rx.len());
                tx.send(2).await.unwrap();
                assert_eq!(2, rx.len());

                assert_eq!(Some(1), rx.recv().await);
                assert_eq!(1, rx.len());
                assert_eq!(Some(2), rx.recv().await);
                assert_eq!(0, rx.len());
                drop(tx);
                assert_eq!(None, rx.recv().await);
                assert_eq!(0, rx.len());
            });
        },
        None,
    );
}

// Sending on a bounded channel doesn't block the sender if the channel isn't filled
#[test]
fn async_mpsc_bounded_sender_buffered() {
    check_dfs(
        || {
            future::block_on(async {
                let (tx, _rx) = mpsc::channel::<i32>(5);
                let handle = future::spawn(async move {
                    for _ in 0..5 {
                        tx.send(1).await.unwrap();
                    }
                    42
                });
                let r = handle.await.unwrap();
                assert_eq!(r, 42);
            });
        },
        None,
    );
}

// Sending on a bounded channel blocks the sender when the channel becomes full
#[test]
#[should_panic(expected = "deadlock")]
fn async_mpsc_bounded_sender_blocked() {
    check_dfs(
        || {
            future::block_on(async {
                let (tx, _rx) = mpsc::channel::<i32>(10);
                let handle = future::spawn(async move {
                    for _ in 0..11 {
                        tx.send(1).await.unwrap();
                    }
                    42
                });
                let r = handle.await.unwrap();
                assert_eq!(r, 42);
            });
        },
        None,
    );
}

async fn mpsc_senders_with_blocking_inner(num_senders: usize, channel_size: usize) {
    assert!(num_senders >= channel_size);
    let num_receives = num_senders - channel_size;
    let (tx, mut rx) = mpsc::channel::<usize>(channel_size);
    let senders = (0..num_senders)
        .map(move |i| {
            let tx = tx.clone();
            future::spawn(async move {
                tx.send(i).await.unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Receive enough messages to ensure no sender will block
    for _ in 0..num_receives {
        let _ = rx.recv().await.unwrap();
    }
    for sender in senders {
        sender.await.unwrap();
    }
}

#[test]
fn async_mpsc_some_senders_with_blocking() {
    check_dfs(
        || {
            future::block_on(async {
                mpsc_senders_with_blocking_inner(3, 1).await;
            });
        },
        None,
    );
}

#[test]
fn async_mpsc_many_senders_with_blocking() {
    shuttle::check_random(
        || {
            future::block_on(async {
                mpsc_senders_with_blocking_inner(1000, 500).await;
            });
        },
        1000,
    );
}

#[test]
fn async_mpsc_many_senders_drop_receiver() {
    const NUM_SENDERS: usize = 3;
    const CHANNEL_SIZE: usize = 1;
    check_dfs(
        || {
            future::block_on(async {
                let (tx, rx) = mpsc::channel::<usize>(CHANNEL_SIZE);
                let senders = (0..NUM_SENDERS)
                    .map(move |i| {
                        let tx = tx.clone();
                        future::spawn(async move {
                            let _ = tx.send(i).await;
                        })
                    })
                    .collect::<Vec<_>>();

                // Drop the receiver; this will unblock any waiting senders
                drop(rx);

                // Make sure all senders finish
                for sender in senders {
                    sender.await.unwrap();
                }
            });
        },
        None,
    );
}

#[test]
fn async_mpsc_multiple_messages() {
    const NUM_MESSAGES: usize = 4;
    check_dfs(
        || {
            future::block_on(async {
                let (tx, mut rx) = mpsc::unbounded_channel();
                let (ctx, crx) = oneshot::channel();

                let h1 = future::spawn(async move {
                    for i in 0..NUM_MESSAGES {
                        tx.send(i).unwrap();
                    }
                    crx.await.unwrap();
                });

                let h2 = future::spawn(async move {
                    for i in 0..NUM_MESSAGES {
                        let n = rx.recv().await.unwrap();
                        assert_eq!(i, n);
                    }
                    ctx.send(()).unwrap();
                });

                futures::future::join_all([h1, h2]).await;
            });
        },
        None,
    );
}

#[test]
fn async_mpsc_select() {
    check_dfs(
        || {
            let lock = std::sync::Arc::new(std::sync::Mutex::new(0));
            let lock2 = lock.clone();
            future::block_on(async move {
                let (tx1, mut rx1) = mpsc::unbounded_channel();
                let (tx2, mut rx2) = mpsc::unbounded_channel();

                let h1 = future::spawn(async move {
                    loop {
                        tokio::select! {
                            biased;

                            msg = rx1.recv() => {
                                if let Some(v) = msg {
                                    *lock2.lock().unwrap() += v;
                                } else {
                                    break;
                                }
                            }

                            _stop = rx2.recv() => {
                                // It is possible to check the branch above, then have a message be sent on `tx1` and `tx2`, then
                                // check this branch.
                                // We have to consume these messages before breaking.
                                while let Some(v) = rx1.recv().await {
                                    *lock2.lock().unwrap() += v;
                                }

                                break;
                            }
                        }
                    }
                });

                tx1.send(10).unwrap();
                tx1.send(32).unwrap();
                tx2.send(()).unwrap();
                drop(tx1);
                h1.await.unwrap();
            });
            let value = *lock.lock().unwrap();
            assert_eq!(value, 42, "{}", value);
        },
        None,
    );
}

#[test]
fn mpsc_drain_after_close() {
    check_dfs(
        || {
            future::block_on(async {
                let (tx, mut rx) = mpsc::unbounded_channel();
                tx.send(()).unwrap();
                rx.close();
                assert!(!rx.is_empty());
                rx.recv()
                    .await
                    .expect("must be able to receive already sent message after closing receiver");
                assert!(rx.is_empty());
                assert!(rx.recv().await.is_none());
            });
        },
        None,
    );
}

#[test]
fn mpsc_send_after_close() {
    check_dfs(
        || {
            future::block_on(async {
                let (tx, mut rx) = mpsc::unbounded_channel();
                rx.close();
                tx.send(())
                    .expect_err("shouldn't be able to send after closing receiver");
                assert!(rx.is_empty());
                assert!(rx.recv().await.is_none());
            });
        },
        None,
    );
}

/// This test captures a pattern that exposed a bug in mpsc.
///
/// The bug has been further reduced to [`mpsc_drain_after_close`] and [`mpsc_send_after_close`],
/// but it doesn't hurt to keep this test.
#[test]
fn mpsc_close_bug() {
    check_random(
        || {
            future::block_on(async {
                async fn send(v: u64, tx: std::sync::Arc<mpsc::UnboundedSender<(u64, oneshot::Sender<()>)>>) -> bool {
                    let (res_tx, res_rx) = oneshot::channel();
                    if tx.send((v, res_tx)).is_ok() {
                        trace!("send {} ok", v);
                        // bug caused potential deadlock here
                        res_rx.await.unwrap();
                        trace!("send {} response", v);
                        true
                    } else {
                        trace!("send {} failed", v);
                        false
                    }
                }

                let (tx, mut rx) = mpsc::unbounded_channel();
                let tx = std::sync::Arc::new(tx);

                let sender1 = future::spawn(send(1, tx.clone()));
                let sender2 = future::spawn(send(2, tx));

                let receiver = future::spawn(async move {
                    let (v, res_tx) = rx.recv().await.unwrap();
                    trace!("first recv {}", v);
                    res_tx.send(()).unwrap();
                    rx.close();
                    if let Some((v, res_tx)) = rx.recv().await {
                        trace!("second recv {}", v);
                        assert!(rx.is_empty());
                        res_tx.send(()).unwrap();
                        true
                    } else {
                        trace!("second recv failed");
                        false
                    }
                });

                let send_1_ok = sender1.await.unwrap();
                let send_2_ok = sender2.await.unwrap();
                let received_both = receiver.await.unwrap();

                assert!(send_1_ok || send_2_ok);
                assert!(!received_both || (send_1_ok && send_2_ok));
            });
        },
        10_000,
    );
}

/// This test captures a pattern that exposed a bug in mpsc.
///
/// Similar to [`mpsc_close_bug`], but dropping instead of closing receiver.
///
/// The bug has been further reduced to [`drop_unreceived_messages_when_receiver_drops`].
#[test]
fn mpsc_drop_receiver_bug() {
    // values collected across executions, to ensure `check_random` explored interesting executions
    let send_results = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    let send_results_ = send_results.clone();

    check_random(
        move || {
            future::block_on(async {
                /// Possible return values:
                /// * `Ok(Ok(()))`: successfully sent message and awaited response
                /// * `Ok(Err(()))`: successfully sent messages but failed awaiting response
                /// * `Err(())`: failed to send message
                async fn send(
                    v: u64,
                    tx: std::sync::Arc<mpsc::UnboundedSender<(u64, oneshot::Sender<()>)>>,
                ) -> Result<Result<(), ()>, ()> {
                    let (res_tx, res_rx) = oneshot::channel();
                    tx.send((v, res_tx)).map_err(|_| ())?;
                    // bug caused potential deadlock here, due to `res_tx` not getting dropped
                    // when `rx` is dropped below
                    Ok(res_rx.await.map_err(|_| ()))
                }

                let (tx, mut rx) = mpsc::unbounded_channel();
                let tx = std::sync::Arc::new(tx);

                let sender1 = future::spawn(send(1, tx.clone()));
                let sender2 = future::spawn(send(2, tx));

                let receiver = future::spawn(async move {
                    let (v, res_tx) = rx.recv().await.unwrap();
                    res_tx.send(()).unwrap();
                    drop(rx);
                    v
                });

                let send_1_res = sender1.await.unwrap();
                let send_2_res = sender2.await.unwrap();
                let received_value = receiver.await.unwrap();

                if received_value == 1 {
                    assert_eq!(send_1_res, Ok(Ok(())));
                    assert_matches!(send_2_res, Err(()) | Ok(Err(())));
                } else {
                    assert_eq!(send_2_res, Ok(Ok(())));
                    assert_matches!(send_1_res, Err(()) | Ok(Err(())));
                }

                let mut send_results = send_results_.lock().unwrap();
                send_results.insert(send_1_res);
                send_results.insert(send_2_res);
            });
        },
        10_000,
    );

    // `Ok(Err(()))` is most important, but let's just check all possible values
    let send_results = Arc::into_inner(send_results).unwrap().into_inner().unwrap();
    assert_eq!(send_results, [Ok(Ok(())), Ok(Err(())), Err(())].into());
}

#[test]
fn drop_unreceived_messages_when_receiver_drops() {
    check_dfs(
        || {
            future::block_on(async {
                let (tx, rx) = mpsc::unbounded_channel();

                let send_task = future::spawn(async move {
                    let (otx, orx) = oneshot::channel::<()>();
                    if tx.send(otx).is_ok() {
                        // used to deadlock due to `otx` not getting dropped when `rx` is dropped below
                        assert!(orx.await.is_err());
                    }
                });

                drop(rx);

                send_task.await.unwrap();
            });
        },
        None,
    );
}

// Sanity test for `is_closed()`. Tests that `close` closes the channel.
#[test]
fn mpsc_close_closes_channel() {
    check_dfs(
        || {
            future::block_on(async {
                let (_tx, mut rx) = mpsc::unbounded_channel::<()>();
                rx.close();
                assert!(rx.is_closed());

                let (_tx, mut rx) = mpsc::channel::<()>(1);
                rx.close();
                assert!(rx.is_closed());
            });
        },
        None,
    );
}

// Sanity test for `is_closed()`, testing that dropping all senders closes the channel.
#[test]
fn mpsc_dropping_senders_closed_channel() {
    check_dfs(
        || {
            future::block_on(async {
                let (unbounded_rx, bounded_rx) = {
                    let (_tx, rx) = mpsc::unbounded_channel::<()>();
                    let (_tx, rx2) = mpsc::channel::<()>(1);
                    (rx, rx2)
                };
                // All senders should be dropped here, meaning the channels should be closed.
                assert!(unbounded_rx.is_closed());
                assert!(bounded_rx.is_closed());
            });
        },
        None,
    );
}

// Tests that `recv`, `blocking_recv` and `try_recv` work correctly on channel drop
#[test]
fn mpsc_recv_and_friends_correct_on_sender_drop_unbounded() {
    // There should be schedules which hit `TryRecvError::Disconnected` and `TryRecvError::Empty`
    let has_seen_empties = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let has_seen_disconnects = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let empties = has_seen_empties.clone();
    let disconnects = has_seen_disconnects.clone();

    check_dfs(
        move || {
            let empties = empties.clone();
            let disconnects = disconnects.clone();

            // The whole idea of these is to have one thread `recv`/`blocking_recv`/`try_recv` and then drop the sender.

            future::block_on(async {
                let jh = {
                    let (_tx, mut rx) = mpsc::unbounded_channel::<()>();
                    future::spawn(async move {
                        let msg = rx.recv().await;
                        assert!(msg.is_none());
                        assert!(rx.is_closed());
                    })
                };
                jh.await.unwrap();

                // The same as above, but with blocking_recv
                let jh = {
                    let (_tx, mut rx) = mpsc::unbounded_channel::<()>();
                    shuttle::thread::spawn(move || {
                        let msg = rx.blocking_recv();
                        assert!(msg.is_none());
                        assert!(rx.is_closed());
                    })
                };
                jh.join().unwrap();

                // Similar, but with `try_recv`.
                let jh = {
                    let (_tx, mut rx) = mpsc::unbounded_channel::<()>();
                    future::spawn(async move {
                        let is_closed = rx.is_closed();

                        match rx.try_recv().unwrap_err() {
                            TryRecvError::Disconnected => {
                                disconnects.clone().store(true, Ordering::SeqCst);
                                assert!(rx.is_closed());
                            }
                            TryRecvError::Empty => {
                                empties.clone().store(true, Ordering::SeqCst);
                                assert!(!is_closed);
                            }
                        }
                    })
                };
                jh.await.unwrap();
            });
        },
        None,
    );

    assert!(has_seen_disconnects.load(Ordering::SeqCst));
    assert!(has_seen_empties.load(Ordering::SeqCst));
}

// Same test as above, but on bounded channels
#[test]
fn mpsc_recv_and_friends_correct_on_sender_drop_bounded() {
    // There should be schedules which hit `TryRecvError::Disconnected` and `TryRecvError::Empty`
    let has_seen_empties = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let has_seen_disconnects = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let empties = has_seen_empties.clone();
    let disconnects = has_seen_disconnects.clone();

    check_dfs(
        move || {
            let empties = empties.clone();
            let disconnects = disconnects.clone();

            // The whole idea of these is to have one thread `recv`/`blocking_recv`/`try_recv` and then drop the sender.

            future::block_on(async {
                let jh = {
                    let (_tx, mut rx) = mpsc::channel::<()>(1);
                    future::spawn(async move {
                        let msg = rx.recv().await;
                        assert!(msg.is_none());
                        assert!(rx.is_closed());
                    })
                };
                jh.await.unwrap();

                // The same as above, but with blocking_recv
                let jh = {
                    let (_tx, mut rx) = mpsc::channel::<()>(1);
                    shuttle::thread::spawn(move || {
                        let msg = rx.blocking_recv();
                        assert!(msg.is_none());
                        assert!(rx.is_closed());
                    })
                };
                jh.join().unwrap();

                // Similar, but with `try_recv`.
                let jh = {
                    let (_tx, mut rx) = mpsc::channel::<()>(1);
                    future::spawn(async move {
                        let is_closed = rx.is_closed();

                        match rx.try_recv().unwrap_err() {
                            TryRecvError::Disconnected => {
                                disconnects.clone().store(true, Ordering::SeqCst);
                                assert!(rx.is_closed());
                            }
                            TryRecvError::Empty => {
                                empties.clone().store(true, Ordering::SeqCst);
                                assert!(!is_closed);
                            }
                        }
                    })
                };
                jh.await.unwrap();
            });
        },
        None,
    );

    assert!(has_seen_disconnects.load(Ordering::SeqCst));
    assert!(has_seen_empties.load(Ordering::SeqCst));
}
