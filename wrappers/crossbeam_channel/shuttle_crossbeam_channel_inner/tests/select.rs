//! Selection under Shuttle: which ready operation is chosen and how, waiting, zero-capacity
//! hand-overs through a selection, and `Select`'s API.

mod common;

use common::{dfs_outcomes, random_outcomes};
use shuttle::{check_dfs, check_random, thread};
use shuttle_crossbeam_channel_inner::{
    Receiver, RecvError, Select, Sender, TryRecvError, bounded, never, select, select_biased, unbounded,
};
use shuttle_time_inner::time::Duration;
use shuttle_time_inner::timer_support::peek;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Two channels with a message each, so that both receive arms are ready.
fn two_ready() -> (Sender<u8>, Receiver<u8>, Sender<u8>, Receiver<u8>) {
    let (s1, r1) = unbounded();
    let (s2, r2) = unbounded();
    s1.send(1).unwrap();
    s2.send(2).unwrap();
    (s1, r1, s2, r2)
}

#[test]
fn select_chooses_among_ready_arms() {
    let chosen = random_outcomes(
        || {
            let (_s1, r1, _s2, r2) = two_ready();
            select! {
                recv(r1) -> msg => msg.unwrap(),
                recv(r2) -> msg => msg.unwrap(),
            }
        },
        100,
    );
    assert_eq!(chosen, [1, 2].into_iter().collect());
}

#[test]
fn a_select_send_arm_and_a_receive_arm_are_both_chosen() {
    let chosen = random_outcomes(
        || {
            let (s1, r1) = unbounded();
            let (s2, r2) = unbounded();
            s1.send(10).unwrap();
            select! {
                recv(r1) -> msg => { assert_eq!(msg, Ok(10)); "recv" }
                send(s2, 20) -> res => {
                    assert_eq!(res, Ok(()));
                    assert_eq!(r2.try_recv(), Ok(20));
                    "send"
                }
            }
        },
        100,
    );
    assert_eq!(chosen, ["recv", "send"].into_iter().collect());
}

// With one ready arm, a selection needs no random data, so it runs under `check_dfs`.
#[test]
fn a_single_ready_arm_draws_nothing() {
    check_dfs(
        || {
            let (s1, r1) = unbounded::<u8>();
            let (_s2, r2) = unbounded::<u8>();
            s1.send(1).unwrap();
            let got = select! {
                recv(r1) -> msg => msg.unwrap(),
                recv(r2) -> _ => unreachable!(),
            };
            assert_eq!(got, 1);
        },
        None,
    );
}

#[test]
#[should_panic(expected = "requested random data from DFS scheduler")]
fn two_ready_arms_draw_from_the_scheduler() {
    check_dfs(
        || {
            let (_s1, r1, _s2, r2) = two_ready();
            select! {
                recv(r1) -> _ => {}
                recv(r2) -> _ => {}
            }
        },
        None,
    );
}

#[test]
fn a_biased_selection_takes_the_first_ready_arm_and_draws_nothing() {
    check_dfs(
        || {
            let (_s1, r1, _s2, r2) = two_ready();
            let got = select_biased! {
                recv(r1) -> msg => msg.unwrap(),
                recv(r2) -> msg => msg.unwrap(),
            };
            assert_eq!(got, 1);
            let mut sel = Select::new_biased();
            sel.recv(&r1);
            let i2 = sel.recv(&r2);
            let oper = sel.select();
            assert_eq!(oper.index(), i2, "r1 is empty now");
            assert_eq!(oper.recv(&r2), Ok(2));
        },
        None,
    );
}

// A selection that has to wait gets whichever message comes first, and in some schedule each
// does.
#[test]
fn a_waiting_selection_gets_whichever_message_comes_first() {
    let winners = dfs_outcomes(|| {
        let (s1, r1) = bounded(1);
        let (s2, r2) = bounded(1);
        let senders = [(s1, 1u8), (s2, 2u8)].map(|(s, n)| thread::spawn(move || s.send(n).unwrap()));
        let first = select! {
            recv(r1) -> msg => msg.unwrap(),
            recv(r2) -> msg => msg.unwrap(),
        };
        for sender in senders {
            sender.join().unwrap();
        }
        first
    });
    assert_eq!(winners, [1, 2].into_iter().collect());
}

// A receiver that arrives while a selection waits with a send arm on a zero-capacity channel claims
// the arm; the selection then works out its message, once, and hands it over.
#[test]
fn a_receiver_claims_a_waiting_selections_send_arm() {
    check_dfs(
        || {
            let (s, r) = bounded::<u32>(0);
            let (_other_s, other_r) = bounded::<u32>(0);
            let evaluated = Arc::new(AtomicUsize::new(0));
            let selector = {
                let evaluated = Arc::clone(&evaluated);
                thread::spawn(move || {
                    select! {
                        send(s, { evaluated.fetch_add(1, Ordering::SeqCst); 42 }) -> res => res.unwrap(),
                        recv(other_r) -> _ => unreachable!("nobody sends on the other channel"),
                    }
                })
            };
            assert_eq!(r.recv(), Ok(42));
            selector.join().unwrap();
            assert_eq!(evaluated.load(Ordering::SeqCst), 1);
        },
        None,
    );
}

// A selection whose send arm finds a receiver waiting on a zero-capacity channel hands it the
// message.
#[test]
fn a_selection_meets_a_waiting_receiver() {
    check_dfs(
        || {
            let (s, r) = bounded::<u32>(0);
            let receiver = thread::spawn(move || r.recv().unwrap());
            let (_never_s, never_r) = bounded::<u32>(0);
            select! {
                send(s, 7) -> res => res.unwrap(),
                recv(never_r) -> _ => unreachable!(),
            }
            assert_eq!(receiver.join().unwrap(), 7);
        },
        None,
    );
}

// Two selections, one sending and one receiving on the same zero-capacity channel, meet, whichever
// waits first.
#[test]
fn two_selections_meet_on_a_zero_capacity_channel() {
    check_dfs(
        || {
            let (s, r) = bounded::<u32>(0);
            let (_s2, r2) = bounded::<u32>(0);
            let (s3, _r3) = bounded::<u32>(0);
            let receiver = thread::spawn(move || {
                select! {
                    recv(r) -> msg => msg.unwrap(),
                    recv(r2) -> _ => unreachable!(),
                }
            });
            select! {
                send(s, 3) -> res => res.unwrap(),
                send(s3, 4) -> _ => unreachable!("nobody receives on the other channel"),
            }
            assert_eq!(receiver.join().unwrap(), 3);
        },
        None,
    );
}

// A task does not rendezvous with itself, even with a send and a receive arm on the same channel.
#[test]
fn a_selection_does_not_meet_itself() {
    check_random(
        || {
            let (s, r) = bounded::<u8>(0);
            let picked = select! {
                send(s, 1) -> _ => "send",
                recv(r) -> _ => "recv",
                default => "neither",
            };
            assert_eq!(picked, "neither");
            let picked = select! {
                send(s, 1) -> _ => "send",
                recv(r) -> _ => "recv",
                default(Duration::from_millis(1)) => "neither",
            };
            assert_eq!(picked, "neither");
        },
        5,
    );
}

#[test]
fn a_disconnected_channel_is_ready() {
    check_random(
        || {
            let (s, r) = unbounded::<u8>();
            drop(s);
            let (_s2, r2) = unbounded::<u8>();
            select! {
                recv(r) -> msg => assert_eq!(msg, Err(RecvError)),
                recv(r2) -> _ => unreachable!(),
            }
            let (s3, r3) = bounded::<u8>(0);
            drop(r3);
            select! {
                send(s3, 1) -> res => assert_eq!(res.unwrap_err().into_inner(), 1),
                recv(r2) -> _ => unreachable!(),
            }
        },
        5,
    );
}

#[test]
#[should_panic(expected = "dropped `SelectedOperation` without completing the operation")]
fn a_selected_operation_must_be_completed() {
    check_random(
        || {
            let (_s, r, _s2, _r2) = two_ready();
            let mut sel = Select::new();
            sel.recv(&r);
            let _ = sel.select();
        },
        1,
    );
}

#[test]
#[should_panic(expected = "passed a receiver that wasn't selected")]
fn a_selected_operation_checks_its_handle() {
    check_random(
        || {
            let (_s, r, _s2, r2) = two_ready();
            let mut sel = Select::new();
            sel.recv(&r);
            let oper = sel.select();
            let _ = oper.recv(&r2);
        },
        1,
    );
}

#[test]
#[should_panic(expected = "no operations have been added to `Select`")]
fn an_empty_selection_panics() {
    check_random(
        || {
            let _ = Select::new().select();
        },
        1,
    );
}

#[test]
fn an_empty_selection_with_a_timeout_sleeps_until_it() {
    check_random(
        || {
            let start = peek();
            assert!(Select::new().select_timeout(Duration::from_millis(5)).is_err());
            assert!(peek() >= start + Duration::from_millis(5));
            assert!(Select::new().try_select().is_err());
            assert!(Select::new().try_ready().is_err());
        },
        5,
    );
}

// `ready` waits for an operation to become ready without performing it.
#[test]
fn ready_waits_without_completing() {
    check_dfs(
        || {
            let (s, r) = bounded::<u8>(1);
            let sender = thread::spawn(move || s.send(5).unwrap());
            let mut sel = Select::new();
            let i = sel.recv(&r);
            assert_eq!(sel.ready(), i);
            assert_eq!(r.try_recv(), Ok(5));
            sender.join().unwrap();
        },
        None,
    );
}

// On a zero-capacity channel, a receive is ready once a sender waits, and a send once a receiver
// does.
#[test]
fn ready_sees_a_waiting_counterpart() {
    check_dfs(
        || {
            let (s, r) = bounded::<u8>(0);
            let sender = thread::spawn(move || s.send(1).unwrap());
            let mut sel = Select::new();
            sel.recv(&r);
            sel.ready();
            assert_eq!(r.try_recv(), Ok(1));
            sender.join().unwrap();

            let (s, r) = bounded::<u8>(0);
            let receiver = thread::spawn(move || r.recv().unwrap());
            let mut sel = Select::new();
            sel.send(&s);
            sel.ready();
            assert!(s.try_send(2).is_ok());
            assert_eq!(receiver.join().unwrap(), 2);
        },
        None,
    );
}

#[test]
fn removed_operations_are_not_selected() {
    check_random(
        || {
            let (_s1, r1, _s2, r2) = two_ready();
            let mut sel = Select::new();
            let i1 = sel.recv(&r1);
            let i2 = sel.recv(&r2);
            sel.remove(i1);
            let oper = sel.select();
            assert_eq!(oper.index(), i2);
            assert_eq!(oper.recv(&r2), Ok(2));
            assert_eq!(r1.try_recv(), Ok(1));
            // The indices of removed operations are not reused.
            let r3 = never::<u8>();
            assert_eq!(sel.recv(&r3), 2);
        },
        20,
    );
}

#[test]
fn try_select_and_try_ready() {
    check_random(
        || {
            let (s, r) = unbounded::<u8>();
            let mut sel = Select::new();
            let i = sel.recv(&r);
            assert!(sel.try_select().is_err());
            assert!(sel.try_ready().is_err());
            s.send(3).unwrap();
            assert_eq!(sel.try_ready(), Ok(i));
            let oper = sel.try_select().unwrap();
            assert_eq!(oper.recv(&r), Ok(3));
            assert_eq!(r.try_recv(), Err(TryRecvError::Empty));
            assert!(sel.clone().try_ready().is_err());
        },
        5,
    );
}

#[test]
fn many_operations_in_one_select() {
    check_random(
        || {
            let channels = (0..6).map(|_| unbounded::<usize>()).collect::<Vec<_>>();
            channels[4].0.send(4).unwrap();
            let mut sel = Select::new();
            for (_, r) in &channels {
                sel.recv(r);
            }
            let oper = sel.select();
            let index = oper.index();
            assert_eq!(oper.recv(&channels[index].1), Ok(4));
        },
        10,
    );
}
