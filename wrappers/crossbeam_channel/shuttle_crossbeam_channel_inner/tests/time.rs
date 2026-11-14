//! Timeouts and timers: a timed wait times out, and a timer fires, only when no task can run, in
//! the order of their deadlines, and moves `shuttle-time`'s clock to the deadline.

mod common;

use common::dfs_outcomes;
use shuttle::sync::atomic::{AtomicBool, Ordering};
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_dfs, check_random, thread};
use shuttle_crossbeam_channel_inner::{
    RecvTimeoutError, Select, SendTimeoutError, after, at, bounded, never, select, tick, unbounded,
};
use shuttle_time_inner::time::{Duration, Instant};

const HOUR: Duration = Duration::from_secs(3600);
const MS: Duration = Duration::from_millis(1);

// A long timeout never fires while the sender can still send, however slow the schedule makes it.
#[test]
fn a_timeout_does_not_fire_while_a_task_can_run() {
    for capacity in [None, Some(0), Some(1)] {
        check_dfs(
            move || {
                let (s, r) = match capacity {
                    None => unbounded(),
                    Some(cap) => bounded(cap),
                };
                let sender = thread::spawn(move || {
                    thread::yield_now();
                    thread::yield_now();
                    s.send(1).unwrap();
                });
                assert_eq!(r.recv_timeout(HOUR), Ok(1));
                sender.join().unwrap();
            },
            None,
        );
    }
}

// Once every other task is blocked, the timeout fires, in every schedule.
#[test]
fn a_timeout_fires_when_no_task_can_run() {
    check_dfs(
        || {
            let (s, r) = unbounded::<u8>();
            let receiver = thread::spawn(move || r.recv_timeout(10 * MS));
            // The main task holds the sender, and blocks until the receiver is done.
            assert_eq!(receiver.join().unwrap(), Err(RecvTimeoutError::Timeout));
            drop(s);
        },
        None,
    );
}

#[test]
fn after_a_timeout_the_clock_reads_its_deadline() {
    check_random(
        || {
            let (_s, r) = unbounded::<u8>();
            let start = Instant::now();
            assert!(r.recv_timeout(10 * MS).is_err());
            let elapsed = start.elapsed();
            assert!(elapsed >= 10 * MS, "{elapsed:?}");
            // Only the clock's own advances of at most a nanosecond per reading come on top.
            assert!(elapsed <= 10 * MS + Duration::from_nanos(4), "{elapsed:?}");
        },
        20,
    );
}

#[test]
fn timeouts_fire_in_the_order_of_their_deadlines() {
    check_dfs(
        || {
            let order = Arc::new(Mutex::new(Vec::new()));
            let (s, r) = unbounded::<u8>();
            let waiters = [30u64, 10, 20]
                .into_iter()
                .map(|millis| {
                    let (order, r) = (Arc::clone(&order), r.clone());
                    thread::spawn(move || {
                        assert!(r.recv_timeout(Duration::from_millis(millis)).is_err());
                        order.lock().unwrap().push(millis);
                    })
                })
                .collect::<Vec<_>>();
            for waiter in waiters {
                waiter.join().unwrap();
            }
            assert_eq!(*order.lock().unwrap(), [10, 20, 30]);
            drop(s);
        },
        None,
    );
}

// A worker that polls a flag between timed receives sees the flag once nothing else can run, rather
// than Shuttle reporting a deadlock.
#[test]
fn a_polling_loop_ends() {
    check_dfs(
        || {
            let stop = Arc::new(AtomicBool::new(false));
            let (s, r) = unbounded::<u8>();
            let worker = {
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    let mut polls = 0;
                    while !stop.load(Ordering::SeqCst) {
                        let _ = r.recv_timeout(100 * MS);
                        polls += 1;
                    }
                    polls
                })
            };
            stop.store(true, Ordering::SeqCst);
            assert!(worker.join().unwrap() <= 1);
            drop(s);
        },
        None,
    );
}

// A loop that waits for a deadline to pass around a timed receive ends: each timeout moves the clock.
#[test]
fn a_deadline_loop_ends() {
    check_random(
        || {
            let (_s, r) = unbounded::<u8>();
            let deadline = Instant::now() + 50 * MS;
            let mut timeouts = 0;
            while Instant::now() < deadline {
                assert!(r.recv_timeout(10 * MS).is_err());
                timeouts += 1;
            }
            assert_eq!(timeouts, 5);
        },
        20,
    );
}

#[test]
fn send_timeouts() {
    check_random(
        || {
            let (s, _r) = bounded(1);
            s.send(1).unwrap();
            assert_eq!(s.send_timeout(2, MS), Err(SendTimeoutError::Timeout(2)));
            let (s, _r) = bounded(0);
            assert_eq!(
                s.send_deadline(3, Instant::now() + MS),
                Err(SendTimeoutError::Timeout(3))
            );
            // A deadline in the past fails at once if the send cannot go on.
            assert_eq!(s.send_deadline(4, Instant::now()), Err(SendTimeoutError::Timeout(4)));
        },
        20,
    );
}

#[test]
fn after_delivers_its_deadline() {
    check_random(
        || {
            let start = Instant::now();
            let fired = after(25 * MS).recv().unwrap();
            assert!(fired >= start + 25 * MS && fired <= start + 25 * MS + Duration::from_nanos(2));
            assert!(Instant::now() >= fired);
            // `at` in the past delivers at once.
            let now = Instant::now();
            let past = at(now);
            assert_eq!(past.try_recv(), Ok(now));
            assert!(past.try_recv().is_err());
            assert_eq!(past.recv_timeout(MS), Err(RecvTimeoutError::Timeout));
        },
        20,
    );
}

#[test]
fn tick_delivers_every_period() {
    check_random(
        || {
            let start = Instant::now();
            let ticker = tick(10 * MS);
            let ticks = (0..4).map(|_| ticker.recv().unwrap()).collect::<Vec<_>>();
            for (k, at) in ticks.iter().enumerate() {
                let expected = start + 10 * MS * (k as u32 + 1);
                assert!(
                    *at >= expected && *at <= expected + Duration::from_nanos(2),
                    "{k}: {at:?}"
                );
            }
            // A late receiver gets the tick that is due, and the next one a period after that.
            let ticker = tick(10 * MS);
            let (_s, r) = unbounded::<u8>();
            assert!(r.recv_timeout(35 * MS).is_err());
            let due = ticker.try_recv().unwrap();
            assert!(ticker.try_recv().is_err());
            let next = ticker.recv().unwrap();
            assert!(next >= due + 35 * MS - 10 * MS, "{due:?} {next:?}");
        },
        20,
    );
}

// Timers in different tasks fire in the order of their delivery times.
#[test]
fn timers_fire_in_the_order_of_their_delivery_times() {
    check_dfs(
        || {
            let order = Arc::new(Mutex::new(Vec::new()));
            let threads = [(20u64, "after 20"), (5, "after 5")]
                .into_iter()
                .map(|(millis, name)| {
                    let order = Arc::clone(&order);
                    thread::spawn(move || {
                        after(Duration::from_millis(millis)).recv().unwrap();
                        order.lock().unwrap().push(name);
                    })
                })
                .chain(std::iter::once({
                    let order = Arc::clone(&order);
                    thread::spawn(move || {
                        let ticker = tick(Duration::from_millis(8));
                        for name in ["tick 8", "tick 16", "tick 24"] {
                            ticker.recv().unwrap();
                            order.lock().unwrap().push(name);
                        }
                    })
                }))
                .collect::<Vec<_>>();
            for thread in threads {
                thread.join().unwrap();
            }
            assert_eq!(
                *order.lock().unwrap(),
                ["after 5", "tick 8", "tick 16", "after 20", "tick 24"]
            );
        },
        None,
    );
}

// The usual crossbeam timeout, a selection with an `after` arm, fires only when no task can run.
#[test]
fn a_selection_with_an_after_arm() {
    let outcomes = dfs_outcomes(|| {
        let (s, r) = bounded::<u8>(1);
        let sender = thread::spawn(move || {
            thread::yield_now();
            s.send(1).unwrap();
        });
        let got = select! {
            recv(r) -> msg => msg.ok(),
            recv(after(HOUR)) -> _ => None,
        };
        sender.join().unwrap();
        got
    });
    assert_eq!(outcomes, [Some(1)].into_iter().collect());

    check_random(
        || {
            let (_s, r) = unbounded::<u8>();
            let start = Instant::now();
            let fired = select! {
                recv(r) -> _ => unreachable!(),
                recv(after(20 * MS)) -> fired => fired.unwrap(),
            };
            assert!(fired >= start + 20 * MS);
        },
        20,
    );
}

#[test]
fn a_selections_default_timeout() {
    check_random(
        || {
            let (_s1, r1) = unbounded::<u8>();
            let (_s2, r2) = unbounded::<u8>();
            let start = Instant::now();
            let picked = select! {
                recv(r1) -> _ => "r1",
                recv(r2) -> _ => "r2",
                default(15 * MS) => "timeout",
            };
            assert_eq!(picked, "timeout");
            assert!(start.elapsed() >= 15 * MS);

            // The earlier of the timeout and an `after` arm wins.
            let picked = select! {
                recv(after(5 * MS)) -> _ => "after",
                recv(r1) -> _ => "r1",
                default(10 * MS) => "timeout",
            };
            assert_eq!(picked, "after");
        },
        20,
    );
}

#[test]
fn ready_timeouts() {
    check_random(
        || {
            let (_s, r) = unbounded::<u8>();
            let mut sel = Select::new();
            sel.recv(&r);
            let start = Instant::now();
            assert!(sel.ready_timeout(7 * MS).is_err());
            assert!(start.elapsed() >= 7 * MS);
            assert!(sel.ready_deadline(Instant::now()).is_err());
            let timer = after(3 * MS);
            let i = sel.recv(&timer);
            assert_eq!(sel.ready(), i);
            assert!(
                sel.select_deadline(Instant::now() + MS)
                    .is_ok_and(|oper| oper.recv(&timer).is_ok())
            );
        },
        20,
    );
}

#[test]
fn never_times_out_only_with_a_deadline() {
    check_random(
        || {
            assert_eq!(never::<u8>().recv_timeout(MS), Err(RecvTimeoutError::Timeout));
        },
        5,
    );
}

#[test]
#[should_panic(expected = "deadlock")]
fn a_receive_from_never_without_a_deadline_deadlocks() {
    check_random(
        || {
            let _ = never::<u8>().recv();
        },
        1,
    );
}

// Timers need no random data, so they work under `check_dfs`.
#[test]
fn timers_work_under_dfs() {
    check_dfs(
        || {
            let (s, r) = bounded::<u8>(0);
            let ticker = tick(MS);
            let worker = thread::spawn(move || {
                for _ in 0..2 {
                    ticker.recv().unwrap();
                }
                s.send_timeout(1, HOUR)
            });
            assert_eq!(r.recv_timeout(HOUR), Ok(1));
            assert!(worker.join().unwrap().is_ok());
            assert!(after(MS).recv().is_ok());
        },
        None,
    );
}
