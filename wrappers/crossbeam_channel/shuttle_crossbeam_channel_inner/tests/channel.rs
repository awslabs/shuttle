//! The channels' semantics under Shuttle: what each kind of channel lets through, when operations
//! block, disconnection, and which task gets a message.

mod common;

use common::{Counted, dfs_outcomes};
use shuttle::sync::atomic::{AtomicBool, Ordering};
use shuttle::{check_dfs, check_random, thread};
use shuttle_crossbeam_channel_inner::{
    RecvError, SendError, SendTimeoutError, TryRecvError, TrySendError, after, bounded, never, tick, unbounded,
};
use shuttle_time_inner::time::Duration;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

#[test]
fn each_senders_messages_arrive_in_order() {
    check_dfs(
        || {
            let (s, r) = unbounded();
            let senders = (0..2)
                .map(|id| {
                    let s = s.clone();
                    thread::spawn(move || {
                        for n in 0..2 {
                            s.send((id, n)).unwrap();
                        }
                    })
                })
                .collect::<Vec<_>>();
            drop(s);
            let received: Vec<(u8, u8)> = r.iter().collect();
            for id in 0..2 {
                let own = received
                    .iter()
                    .filter(|(from, _)| *from == id)
                    .map(|(_, n)| *n)
                    .collect::<Vec<_>>();
                assert_eq!(own, [0, 1]);
            }
            for sender in senders {
                sender.join().unwrap();
            }
        },
        None,
    );
}

// Two receivers wait on one message, and in some schedule each of them gets it: crossbeam does not
// promise which one does, so Shuttle explores both.
#[test]
fn any_waiting_receiver_can_get_a_message() {
    for capacity in [None, Some(1)] {
        let winners = dfs_outcomes(move || {
            let (s, r) = match capacity {
                None => unbounded(),
                Some(cap) => bounded(cap),
            };
            let receivers = (0..2)
                .map(|id| {
                    let r = r.clone();
                    thread::spawn(move || r.recv().ok().map(|()| id))
                })
                .collect::<Vec<_>>();
            s.send(()).unwrap();
            drop(s);
            let got = receivers
                .into_iter()
                .filter_map(|receiver| receiver.join().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(got.len(), 1, "exactly one receiver gets the message");
            got[0]
        });
        assert_eq!(winners, [0, 1].into_iter().collect(), "capacity {capacity:?}");
    }
}

#[test]
fn a_full_bounded_channel_blocks_its_sender() {
    check_dfs(
        || {
            let (s, r) = bounded(1);
            s.send(1).unwrap();
            assert!(s.is_full());
            assert_eq!(s.try_send(2), Err(TrySendError::Full(2)));
            let sender = thread::spawn(move || {
                s.send(2).unwrap();
                s.send(3).unwrap();
            });
            assert_eq!(r.iter().take(3).collect::<Vec<_>>(), [1, 2, 3]);
            sender.join().unwrap();
        },
        None,
    );
}

// A zero-capacity send completes only once a receiver has arrived for it, in every schedule.
#[test]
fn a_zero_capacity_send_waits_for_its_receiver() {
    check_dfs(
        || {
            let (s, r) = bounded(0);
            let arrived = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let receiver = {
                let arrived = Arc::clone(&arrived);
                thread::spawn(move || {
                    thread::yield_now();
                    // Not a scheduling point, so this and the receive start in the same step.
                    arrived.store(true, std::sync::atomic::Ordering::SeqCst);
                    r.recv().unwrap()
                })
            };
            s.send(5).unwrap();
            assert!(arrived.load(std::sync::atomic::Ordering::SeqCst));
            assert_eq!(receiver.join().unwrap(), 5);
        },
        None,
    );
}

#[test]
fn a_zero_capacity_channel_needs_both_sides_at_once() {
    check_random(
        || {
            let (s, r) = bounded::<u8>(0);
            assert_eq!(s.try_send(1), Err(TrySendError::Full(1)));
            assert_eq!(r.try_recv(), Err(TryRecvError::Empty));
            assert!(s.is_empty() && s.is_full());
            assert_eq!((s.len(), s.capacity()), (0, Some(0)));

            // A blocked receiver is there for `try_send` to meet.
            let receiver = thread::spawn(move || r.recv().unwrap());
            loop {
                match s.try_send(9) {
                    Ok(()) => break,
                    Err(TrySendError::Full(_)) => thread::yield_now(),
                    Err(TrySendError::Disconnected(_)) => unreachable!(),
                }
            }
            assert_eq!(receiver.join().unwrap(), 9);
        },
        50,
    );
}

#[test]
fn a_blocked_zero_capacity_send_is_there_for_try_recv() {
    check_random(
        || {
            let (s, r) = bounded::<u8>(0);
            let sender = thread::spawn(move || s.send(4).unwrap());
            let received = loop {
                match r.try_recv() {
                    Ok(value) => break value,
                    Err(TryRecvError::Empty) => thread::yield_now(),
                    Err(TryRecvError::Disconnected) => unreachable!(),
                }
            };
            assert_eq!(received, 4);
            sender.join().unwrap();
        },
        50,
    );
}

#[test]
fn dropping_the_last_sender_wakes_blocked_receivers() {
    for capacity in [None, Some(0), Some(2)] {
        check_dfs(
            move || {
                let (s, r) = match capacity {
                    None => unbounded::<u8>(),
                    Some(cap) => bounded(cap),
                };
                let receivers = (0..2)
                    .map(|_| {
                        let r = r.clone();
                        thread::spawn(move || r.recv())
                    })
                    .collect::<Vec<_>>();
                drop(s);
                for receiver in receivers {
                    assert_eq!(receiver.join().unwrap(), Err(RecvError));
                }
                assert_eq!(r.try_recv(), Err(TryRecvError::Disconnected));
            },
            None,
        );
    }
}

#[test]
fn dropping_the_last_receiver_wakes_blocked_senders_with_their_messages() {
    for capacity in [0, 1] {
        check_dfs(
            move || {
                let (s, r) = bounded::<u8>(capacity);
                if capacity == 1 {
                    s.send(0).unwrap();
                }
                let sender = {
                    let s = s.clone();
                    thread::spawn(move || s.send(7))
                };
                let timed = thread::spawn(move || s.send_timeout(8, Duration::from_secs(3600)));
                drop(r);
                assert_eq!(sender.join().unwrap(), Err(SendError(7)));
                assert_eq!(timed.join().unwrap(), Err(SendTimeoutError::Disconnected(8)));
            },
            None,
        );
    }
}

#[test]
fn messages_sent_before_the_disconnection_still_arrive() {
    check_dfs(
        || {
            let (s, r) = bounded(2);
            let sender = thread::spawn(move || {
                s.send(1).unwrap();
                s.send(2).unwrap();
            });
            assert_eq!(r.recv(), Ok(1));
            assert_eq!(r.recv(), Ok(2));
            assert_eq!(r.recv(), Err(RecvError));
            sender.join().unwrap();
        },
        None,
    );
}

// As crossbeam's, an unbounded channel drops its messages when the last receiver goes; a bounded
// one keeps them until the last handle goes.
#[test]
fn when_a_channel_drops_its_messages() {
    check_random(
        || {
            let drops = Arc::new(AtomicUsize::new(0));
            let (s, r) = unbounded();
            s.send(Counted(Arc::clone(&drops))).unwrap();
            s.send(Counted(Arc::clone(&drops))).unwrap();
            drop(r);
            assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 2);
            assert!(s.send(Counted(Arc::clone(&drops))).is_err());
            assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 3);

            let drops = Arc::new(AtomicUsize::new(0));
            let (s, r) = bounded(2);
            s.send(Counted(Arc::clone(&drops))).unwrap();
            drop(r);
            assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 0);
            drop(s);
            assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 1);
        },
        5,
    );
}

#[test]
fn handles_compare_by_channel() {
    check_random(
        || {
            let (s, r) = unbounded::<u8>();
            let (s2, r2) = unbounded::<u8>();
            assert!(s.same_channel(&s.clone()));
            assert!(!s.same_channel(&s2));
            assert!(r.same_channel(&r.clone()));
            assert!(!r.same_channel(&r2));
            assert!(never::<u8>().same_channel(&never()));
            assert!(!r.same_channel(&never()));
            let a = after(Duration::from_millis(1));
            assert!(a.same_channel(&a.clone()));
            assert!(!a.same_channel(&after(Duration::from_millis(1))));
            let t = tick(Duration::from_millis(1));
            assert!(t.same_channel(&t.clone()));
            assert!(!t.same_channel(&a));
        },
        5,
    );
}

#[test]
fn the_capacities_and_lengths_of_timers_and_never() {
    check_random(
        || {
            let a = after(Duration::from_secs(1));
            assert_eq!(
                (a.capacity(), a.len(), a.is_empty(), a.is_full()),
                (Some(1), 0, true, false)
            );
            let t = tick(Duration::from_secs(1));
            assert_eq!((t.capacity(), t.len()), (Some(1), 0));
            let n = never::<u8>();
            assert_eq!(
                (n.capacity(), n.len(), n.is_empty(), n.is_full()),
                (Some(0), 0, true, true)
            );
            assert_eq!(n.try_recv(), Err(TryRecvError::Empty));
            // Once the clock reaches the delivery time, the message is there.
            a.recv().unwrap();
            assert!(a.is_empty());
        },
        5,
    );
}

// Clones of a receiver share one channel, also for a timer, which delivers its one message once.
#[test]
fn a_cloned_after_delivers_once() {
    check_random(
        || {
            let a = after(Duration::from_millis(5));
            let b = a.clone();
            let first = a.recv().unwrap();
            assert_eq!(b.try_recv(), Err(TryRecvError::Empty));
            assert!(b.recv_timeout(Duration::from_millis(1)).is_err());
            let _ = first;
        },
        5,
    );
}

#[test]
fn a_shared_flag_and_channels() {
    // A flag set before the send is visible after the receive: the message carries causality.
    check_dfs(
        || {
            let flag = Arc::new(AtomicBool::new(false));
            let (s, r) = bounded(0);
            let sender = {
                let flag = Arc::clone(&flag);
                thread::spawn(move || {
                    flag.store(true, Ordering::SeqCst);
                    s.send(()).unwrap();
                })
            };
            r.recv().unwrap();
            assert!(flag.load(Ordering::SeqCst));
            sender.join().unwrap();
        },
        None,
    );
}
