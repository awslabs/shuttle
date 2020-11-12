use shuttle::sync::{channel, sync_channel, RecvError};
use shuttle::{check_dfs, thread};
use test_env_log::test;

// The following tests (prefixed with mpsc_loom) are from the
// loom test suite; see https://github.com/tokio-rs/loom/blob/master/tests/mpsc.rs
#[test]
fn mpsc_loom_basic_sequential_usage() {
    check_dfs(
        move || {
            let (s, r) = channel();
            s.send(5).unwrap();
            let val = r.recv().unwrap();
            assert_eq!(val, 5);
        },
        None,
        None,
    );
}

#[test]
fn mpsc_loom_basic_parallel_usage() {
    check_dfs(
        || {
            let (s, r) = channel();
            thread::spawn(move || {
                s.send(5).unwrap();
            });
            let val = r.recv().unwrap();
            assert_eq!(val, 5);
        },
        None,
        None,
    );
}

#[test]
fn mpsc_loom_commutative_senders() {
    check_dfs(
        || {
            let (s, r) = channel();
            let s2 = s.clone();
            thread::spawn(move || {
                s.send(5).unwrap();
            });
            thread::spawn(move || {
                s2.send(6).unwrap();
            });
            let mut val = r.recv().unwrap();
            val += r.recv().unwrap();
            assert_eq!(val, 11);
        },
        None,
        None,
    );
}

fn ignore_result<A, B>(_: Result<A, B>) {}

#[test]
#[should_panic]
fn mpsc_loom_non_commutative_senders1() {
    check_dfs(
        || {
            let (s, r) = channel();
            let s2 = s.clone();
            thread::spawn(move || {
                ignore_result(s.send(5));
            });
            thread::spawn(move || {
                ignore_result(s2.send(6));
            });
            let val = r.recv().unwrap();
            assert_eq!(val, 5);
            ignore_result(r.recv());
        },
        None,
        None,
    );
}

#[test]
#[should_panic]
fn mpsc_loom_non_commutative_senders2() {
    check_dfs(
        || {
            let (s, r) = channel();
            let s2 = s.clone();
            thread::spawn(move || {
                ignore_result(s.send(5));
            });
            thread::spawn(move || {
                ignore_result(s2.send(6));
            });
            let val = r.recv().unwrap();
            assert_eq!(val, 6);
            ignore_result(r.recv());
        },
        None,
        None,
    );
}

#[test]
fn mpsc_drop_sender_unbounded() {
    check_dfs(
        || {
            let (tx, rx) = channel::<i32>();
            thread::spawn(move || {
                drop(tx);
            });
            assert!(rx.recv().is_err());
        },
        None,
        None,
    );
}

#[test]
fn mpsc_drop_receiver_unbounded() {
    check_dfs(
        || {
            let (tx, rx) = channel();
            drop(rx);
            assert!(tx.send(1).is_err());
        },
        None,
        None,
    );
}

#[test]
fn mpsc_drop_sender_bounded() {
    check_dfs(
        || {
            let (tx, rx) = sync_channel::<i32>(10);
            thread::spawn(move || {
                assert!(rx.recv().is_err());
            });
            drop(tx);
        },
        None,
        None,
    );
}

#[test]
fn mpsc_drop_receiver_bounded() {
    check_dfs(
        || {
            let (tx, rx) = sync_channel(10);
            drop(rx);
            assert!(tx.send(1).is_err());
        },
        None,
        None,
    );
}

#[test]
fn mpsc_drop_sender_rendezvous() {
    check_dfs(
        || {
            let (tx, rx) = sync_channel::<i32>(0);
            drop(tx);
            assert!(rx.recv().is_err());
        },
        None,
        None,
    );
}

#[test]
fn mpsc_drop_receiver_rendezvous() {
    check_dfs(
        || {
            let (tx, rx) = sync_channel(0);
            drop(rx);
            assert!(tx.send(1).is_err());
        },
        None,
        None,
    );
}

// Example taken from the std::sync::mpsc documentation
// See "buffering behavior" example in
//   https://doc.rust-lang.org/std/sync/mpsc/struct.Receiver.html
#[test]
fn mpsc_buffering_behavior() {
    check_dfs(
        || {
            let (send, recv) = channel();
            let handle = thread::spawn(move || {
                send.send(1u8).unwrap();
                send.send(2).unwrap();
                send.send(3).unwrap();
                drop(send);
            });

            // wait for the thread to join so we ensure the sender is dropped
            handle.join().unwrap();

            // values sent before the sender disconnects are still available afterwards
            assert_eq!(Ok(1), recv.recv());
            assert_eq!(Ok(2), recv.recv());
            assert_eq!(Ok(3), recv.recv());
            // but after the values are exhausted, recv() returns an error
            assert_eq!(Err(RecvError), recv.recv());
        },
        None,
        None,
    );
}

#[test]
fn mpsc_bounded_sum() {
    check_dfs(
        || {
            let (tx, rx) = sync_channel::<i32>(5);
            thread::spawn(move || {
                for _ in 0..5 {
                    tx.send(1).unwrap();
                }
            });
            let handle = thread::spawn(move || {
                let mut sum = 0;
                for _ in 0..5 {
                    sum += rx.recv().unwrap();
                }
                sum
            });
            let r = handle.join().unwrap();
            assert_eq!(r, 5);
        },
        None,
        None,
    );
}

// Sending on a bounded channel doesn't block the sender if the channel isn't filled
#[test]
fn mpsc_bounded_sender_buffered() {
    check_dfs(
        || {
            let (tx, _rx) = sync_channel::<i32>(10);
            let handle = thread::spawn(move || {
                for _ in 0..10 {
                    tx.send(1).unwrap();
                }
                42
            });
            let r = handle.join().unwrap();
            assert_eq!(r, 42);
        },
        None,
        None,
    );
}

// Sending on a bounded channel blocks the sender when the channel becomes full
#[test]
#[should_panic(expected = "deadlock")]
fn mpsc_bounded_sender_blocked() {
    check_dfs(
        || {
            let (tx, _rx) = sync_channel::<i32>(10);
            let handle = thread::spawn(move || {
                for _ in 0..11 {
                    tx.send(1).unwrap();
                }
                42
            });
            let r = handle.join().unwrap();
            assert_eq!(r, 42);
        },
        None,
        None,
    );
}

// The following set of tests (prefixed `mpsc_rendezvous_`) check rendezvous channels.

#[test]
fn mpsc_rendezvous_channel() {
    check_dfs(
        || {
            let (tx, rx) = sync_channel::<i32>(0);
            thread::spawn(move || {
                // This will wait for the parent thread to start receiving
                tx.send(53).unwrap();
            });
            let v = rx.recv().unwrap();
            assert_eq!(v, 53);
        },
        None,
        None,
    );
}

#[test]
#[should_panic(expected = "deadlock")]
fn mpsc_rendezvous_sender_block() {
    check_dfs(
        || {
            let (tx, rx) = sync_channel::<i32>(0);
            tx.send(53).unwrap();
            rx.recv().unwrap();
            rx.recv().unwrap();
        },
        None,
        None,
    );
}

#[test]
fn mpsc_rendezvous_two_threads() {
    check_dfs(
        || {
            let (tx1, rx) = sync_channel::<i32>(0);
            let tx2 = tx1.clone();
            thread::spawn(move || {
                tx1.send(10).unwrap();
            });
            thread::spawn(move || {
                tx2.send(20).unwrap();
            });
            let v1 = rx.recv().unwrap();
            let v2 = rx.recv().unwrap();
            assert_eq!(v1 + v2, 30);
        },
        None,
        None,
    );
}

// An mpsc Receiver is not clone-able and is !Sync, so it can't be shared, but
// it is Send, so it can be transferred between threads. In this example, we
// rendezvous two separate threads with the main thread by passing the receiver
// to the rendezvous channel using a second, bounded channel.
#[test]
fn mpsc_rendezvous_transfer_receiver() {
    check_dfs(
        || {
            // First channel is used to send the receiver from one thread to another
            let (tx1, rx1) = sync_channel(1);

            // Second channel is a rendezvous channel used to synchronize with the main thread
            let (tx2, rx2) = sync_channel::<i32>(0);

            thread::spawn(move || {
                let p = rx2.recv().unwrap();
                assert_eq!(p, 10);
                // Send the receiver to the 2nd thread
                tx1.send(rx2).unwrap();
            });

            let handle = thread::spawn(move || {
                let rx2 = rx1.recv().unwrap();
                let q = rx2.recv().unwrap();
                assert_eq!(q, 20);
            });

            tx2.send(10).unwrap();
            tx2.send(20).unwrap();

            // Wait for the 2nd thread to finish
            handle.join().unwrap();
        },
        None,
        None,
    );
}

// From libstd test suite
#[test]
fn mpsc_send_from_outside_runtime() {
    check_dfs(
        || {
            let (tx1, rx1) = channel::<()>();
            let (tx2, rx2) = channel::<i32>();
            let t1 = thread::spawn(move || {
                tx1.send(()).unwrap();
                for _ in 0..7 {
                    assert_eq!(rx2.recv().unwrap(), 1);
                }
            });
            rx1.recv().unwrap();
            let t2 = thread::spawn(move || {
                for _ in 0..7 {
                    tx2.send(1).unwrap();
                }
            });
            t1.join().expect("thread panicked");
            t2.join().expect("thread panicked");
        },
        None,
        None,
    );
}

// From libstd test suite
#[test]
fn mpsc_recv_from_outside_runtime() {
    check_dfs(
        || {
            let (tx, rx) = channel::<i32>();
            let t = thread::spawn(move || {
                for _ in 0..10 {
                    assert_eq!(rx.recv().unwrap(), 1);
                }
            });
            for _ in 0..10 {
                tx.send(1).unwrap();
            }
            t.join().expect("thread panicked");
        },
        None,
        None,
    );
}

// From libstd test suite
// TODO This test actually should not panic, because calling join on a child thread that panics
// TODO should return an Err with the parameter given to the panic
// TODO See documentation for `join` in https://doc.rust-lang.org/std/thread/struct.JoinHandle.html
#[test]
#[should_panic]
fn mpsc_oneshot_single_thread_recv_chan_close() {
    check_dfs(
        || {
            // Receiving on a closed chan will panic
            let res = thread::spawn(move || {
                let (tx, rx) = channel::<i32>();
                drop(tx);
                rx.recv().unwrap();
            })
            .join();
            // What is our res?
            assert!(res.is_err());
        },
        None,
        None,
    );
}

#[test]
fn mpsc_many_senders_with_blocking() {
    const NUM_SENDERS: usize = 4;
    const CHANNEL_SIZE: usize = 2;
    const NUM_RECEIVES: usize = NUM_SENDERS - CHANNEL_SIZE;
    check_dfs(
        || {
            let (tx, rx) = sync_channel::<usize>(CHANNEL_SIZE);
            let senders = (0..NUM_SENDERS)
                .map(move |i| {
                    let tx = tx.clone();
                    thread::spawn(move || {
                        tx.send(i).unwrap();
                    })
                })
                .collect::<Vec<_>>();

            // Receive enough messages to ensure no sender will block
            for _ in 0..NUM_RECEIVES {
                rx.recv().unwrap();
            }
            for sender in senders {
                sender.join().unwrap();
            }
        },
        None,
        None,
    );
}

#[test]
fn mpsc_many_senders_drop_receiver() {
    const NUM_SENDERS: usize = 4;
    const CHANNEL_SIZE: usize = 2;
    check_dfs(
        || {
            let (tx, rx) = sync_channel::<usize>(CHANNEL_SIZE);
            let senders = (0..NUM_SENDERS)
                .map(move |i| {
                    let tx = tx.clone();
                    thread::spawn(move || {
                        let _ = tx.send(i);
                    })
                })
                .collect::<Vec<_>>();

            // Drop the receiver; this will unblock any waiting senders
            drop(rx);

            // Make sure all senders finish
            for sender in senders {
                sender.join().unwrap();
            }
        },
        None,
        None,
    );
}

/*
** TODO: We don't support iter() on receivers yet
**
#[test]
fn test_nested_recv_iter() {
    check_dfs(|| {
        let (tx, rx) = sync_channel::<i32>(0);
        let (total_tx, total_rx) = sync_channel::<i32>(0);

        let _t = thread::spawn(move || {
            let mut acc = 0;
            for x in rx.iter() {
                acc += x;
            }
            total_tx.send(acc).unwrap();
        });

        tx.send(3).unwrap();
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        drop(tx);
        assert_eq!(total_rx.recv().unwrap(), 6);
    }, None, None);
}
*/

/*
** TODO: try_recv() not yet supported
#[test]
fn mpsc_oneshot_single_thread_peek_close() {
    check_dfs(|| {
        let (tx, rx) = channel::<i32>();
        drop(tx);
        assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
        assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
    }, None, None);
}
*/
