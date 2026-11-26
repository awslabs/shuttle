//! Tests for `sync::Barrier`.

use futures::FutureExt;
use shuttle::sync::atomic::{AtomicBool, AtomicUsize};
use shuttle::sync::Arc as ShuttleArc;
use shuttle::{check_dfs, check_random_with_seed, check_uncontrolled_nondeterminism, future};
use shuttle_tokio_impl_inner::sync::{mpsc, Barrier, BarrierWaitResult};
use std::collections::BTreeSet;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use test_log::test;

/// Has `n` tasks rendezvous on a barrier of `n`, and returns how many of them were the leader.
///
/// Each task counts itself in before it waits and checks, once the wait is over, that every task
/// has counted itself in. That is what the barrier promises: no task gets past it until all of
/// them have reached it.
async fn rendezvous(n: usize) -> usize {
    let barrier = Arc::new(Barrier::new(n));
    let arrived = ShuttleArc::new(AtomicUsize::new(0));

    let tasks = (0..n)
        .map(|_| {
            let (barrier, arrived) = (barrier.clone(), arrived.clone());
            future::spawn(async move {
                arrived.fetch_add(1, Ordering::SeqCst);
                let result = barrier.wait().await;
                assert_eq!(
                    arrived.load(Ordering::SeqCst),
                    n,
                    "a task got past the barrier before every task reached it"
                );
                result.is_leader()
            })
        })
        .collect::<Vec<_>>();

    let mut leaders = 0;
    for task in tasks {
        leaders += usize::from(task.await.unwrap());
    }
    leaders
}

#[test]
fn releases_every_task_and_picks_one_leader() {
    check_dfs(|| assert_eq!(future::block_on(rendezvous(2)), 1), None);
}

/// Exhaustively, three tasks take about 8 seconds, so this samples schedules instead.
#[test]
fn releases_every_task_and_picks_one_leader_with_three() {
    check_random_with_seed(|| assert_eq!(future::block_on(rendezvous(3)), 1), 0x5eed, 3000);
}

/// A barrier is reusable, and each generation has a leader of its own.
#[test]
fn is_reusable() {
    check_dfs(
        || {
            future::block_on(async {
                let barrier = Arc::new(Barrier::new(2));
                let leaders = ShuttleArc::new(AtomicUsize::new(0));

                for _ in 0..2 {
                    let tasks = (0..2)
                        .map(|_| {
                            let (barrier, leaders) = (barrier.clone(), leaders.clone());
                            future::spawn(async move {
                                if barrier.wait().await.is_leader() {
                                    leaders.fetch_add(1, Ordering::SeqCst);
                                }
                            })
                        })
                        .collect::<Vec<_>>();
                    for task in tasks {
                        task.await.unwrap();
                    }
                }

                assert_eq!(leaders.load(Ordering::SeqCst), 2);
            })
        },
        None,
    );
}

/// More tasks than the barrier's size at once: they are released in generations of `n`, with a
/// leader each, however their arrivals interleave. Exhaustively, this takes about 8 seconds, so it
/// samples schedules instead.
#[test]
fn concurrent_generations() {
    check_random_with_seed(
        || {
            future::block_on(async {
                let barrier = Arc::new(Barrier::new(2));
                let tasks = (0..4)
                    .map(|_| {
                        let barrier = barrier.clone();
                        future::spawn(async move { barrier.wait().await.is_leader() })
                    })
                    .collect::<Vec<_>>();
                let mut leaders = 0;
                for task in tasks {
                    leaders += usize::from(task.await.unwrap());
                }
                assert_eq!(leaders, 2);
            })
        },
        0xbeef,
        3000,
    );
}

/// As in `std::sync::Barrier` and in tokio, a barrier of 0 behaves like one of 1: every wait
/// resolves at once, and every waiter is the leader of its own generation.
#[test]
fn zero_is_like_one() {
    for n in [0, 1] {
        check_dfs(
            move || {
                future::block_on(async {
                    let barrier = Barrier::new(n);
                    assert!(barrier.wait().await.is_leader());
                    assert!(barrier.wait().await.is_leader());
                })
            },
            None,
        );
    }
}

/// Which task is the leader is the scheduler's to decide, so Shuttle must explore every choice.
#[test]
fn every_task_can_be_the_leader() {
    let leaders = Arc::new(Mutex::new(BTreeSet::new()));
    let seen = leaders.clone();

    check_dfs(
        move || {
            let seen = seen.clone();
            future::block_on(async move {
                let barrier = Arc::new(Barrier::new(3));
                let tasks = (0..3)
                    .map(|id| {
                        let barrier = barrier.clone();
                        future::spawn(async move { barrier.wait().await.is_leader().then_some(id) })
                    })
                    .collect::<Vec<_>>();
                for task in tasks {
                    if let Some(id) = task.await.unwrap() {
                        seen.lock().unwrap().insert(id);
                    }
                }
            })
        },
        None,
    );

    assert_eq!(*leaders.lock().unwrap(), BTreeSet::from([0, 1, 2]));
}

/// Regression test: Shuttle must be able to schedule another task between a task's last operation
/// before `wait` and its arrival, whether that arrival will wait or release the generation.
///
/// One task stores a flag and then waits; the other reads the flag and then waits. Which of them
/// arrives last, and so is the leader, is independent of what the other read: with threads, all
/// four combinations happen. Two of them need the other task to run between the first one's store
/// and its arrival: reading `true` and still arriving first, or reading `false` and arriving last.
/// A model that takes no scheduling point before an arrival that will wait explores only the other
/// two.
#[test]
fn every_arrival_is_a_separate_step() {
    let outcomes = Arc::new(Mutex::new(BTreeSet::new()));
    let seen = outcomes.clone();

    check_dfs(
        move || {
            let seen = seen.clone();
            future::block_on(async move {
                let barrier = Arc::new(Barrier::new(2));
                let flag = ShuttleArc::new(AtomicBool::new(false));
                let other = {
                    let (barrier, flag) = (barrier.clone(), flag.clone());
                    future::spawn(async move {
                        let saw_flag = flag.load(Ordering::SeqCst);
                        barrier.wait().await;
                        saw_flag
                    })
                };
                flag.store(true, Ordering::SeqCst);
                let storer_is_leader = barrier.wait().await.is_leader();
                let saw_flag = other.await.unwrap();
                seen.lock().unwrap().insert((saw_flag, storer_is_leader));
            })
        },
        None,
    );

    assert_eq!(
        *outcomes.lock().unwrap(),
        BTreeSet::from([(false, false), (false, true), (true, false), (true, true)])
    );
}

/// The barrier holds every task back until the `n`th arrives, so with one task too few they all
/// wait forever, and Shuttle reports it.
#[test]
#[should_panic(expected = "deadlock")]
fn too_few_tasks_deadlock() {
    check_dfs(
        || {
            future::block_on(async {
                let barrier = Arc::new(Barrier::new(3));
                let other = {
                    let barrier = barrier.clone();
                    future::spawn(async move { barrier.wait().await })
                };
                barrier.wait().await;
                other.await.unwrap();
            })
        },
        None,
    );
}

/// The barrier orders what comes before it in every task before what comes after it in every
/// task, and nothing more: Shuttle must still find the bug in a test program that assumes that
/// what another task does after the barrier is done by the time this one is past it.
#[test]
#[should_panic(expected = "the other task has not stored its flag yet")]
fn nothing_is_ordered_after_the_barrier() {
    check_dfs(
        || {
            future::block_on(async {
                let barrier = Arc::new(Barrier::new(2));
                let flag = ShuttleArc::new(AtomicBool::new(false));
                let other = {
                    let (barrier, flag) = (barrier.clone(), flag.clone());
                    future::spawn(async move {
                        barrier.wait().await;
                        flag.store(true, Ordering::SeqCst);
                    })
                };
                barrier.wait().await;
                // BUG (in this test program): the other task may not have run on yet.
                assert!(
                    flag.load(Ordering::SeqCst),
                    "the other task has not stored its flag yet"
                );
                other.await.unwrap();
            })
        },
        None,
    );
}

/// Waiting does not block the Shuttle task, so a task can wait on the same barrier twice at once,
/// as it can with tokio's.
#[test]
fn one_task_can_wait_twice_at_once() {
    check_dfs(
        || {
            future::block_on(async {
                let barrier = Barrier::new(2);
                let (first, second) = futures::join!(barrier.wait(), barrier.wait());
                assert_eq!(usize::from(first.is_leader()) + usize::from(second.is_leader()), 1);
            })
        },
        None,
    );
}

/// The scenario of `cancelled_wait_still_counts`, run against tokio's barrier.
#[test]
fn tokio_cancelled_wait_still_counts() {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    runtime.block_on(async {
        let barrier = tokio::sync::Barrier::new(2);
        {
            let mut wait = Box::pin(barrier.wait());
            assert!(wait.as_mut().now_or_never().is_none());
        }
        // The dropped wait still counts, so this one is the second arrival.
        assert!(barrier.wait().now_or_never().expect("released at once").is_leader());
    });
}

/// As in tokio (see `tokio_cancelled_wait_still_counts`), `wait` is not cancel safe: an arrival
/// counts from the first poll of its `wait` on, even if the `wait` is dropped before the
/// generation is released.
#[test]
fn cancelled_wait_still_counts() {
    check_dfs(
        || {
            future::block_on(async {
                let barrier = Barrier::new(2);
                {
                    let mut wait = Box::pin(barrier.wait());
                    assert!(wait.as_mut().now_or_never().is_none());
                }
                assert!(barrier.wait().now_or_never().expect("released at once").is_leader());
            })
        },
        None,
    );
}

/// A wait that a `select!` drops, whether before or after its arrival, leaves the barrier in a
/// state from which the other task gets past it, and exactly one of the waits that complete is the
/// leader.
#[test]
fn wait_dropped_by_select() {
    check_dfs(
        || {
            future::block_on(async {
                let barrier = Arc::new(Barrier::new(2));
                let (tx, mut rx) = mpsc::unbounded_channel::<()>();

                let selecting = {
                    let barrier = barrier.clone();
                    future::spawn(async move {
                        shuttle_tokio_impl_inner::select! {
                            biased;
                            result = barrier.wait() => Some(result.is_leader()),
                            _ = rx.recv() => None,
                        }
                    })
                };

                tx.send(()).unwrap();
                let mine = barrier.wait().await.is_leader();
                // The `select!` is biased towards the wait, so the selecting task always arrives,
                // on its first poll, and the two arrivals make one generation.
                match selecting.await.unwrap() {
                    // Both waits completed, so exactly one of them was the leader.
                    Some(theirs) => assert_ne!(theirs, mine),
                    // The selecting task took the message after it arrived and dropped its wait,
                    // so the second arrival, and the leader, was this task's.
                    None => assert!(mine),
                }
            })
        },
        None,
    );
}

/// A `Barrier`, its `wait` future and its result can be shared and sent, as tokio's can.
#[test]
fn auto_traits() {
    fn send_sync<T: Send + Sync>() {}
    fn send_sync_value<T: Send + Sync>(_: &T) {}
    send_sync::<Barrier>();
    send_sync::<BarrierWaitResult>();
    // Creating the future runs none of it, so this needs no Shuttle test.
    send_sync_value(&Barrier::new(1).wait());
}

#[test]
fn debug() {
    check_dfs(
        || {
            future::block_on(async {
                let barrier = Barrier::new(2);
                assert_eq!(
                    format!("{barrier:?}"),
                    "Barrier { n: 2, arrived: 0, generation: 0, .. }"
                );
                let mut wait = Box::pin(barrier.wait());
                assert!(wait.as_mut().now_or_never().is_none());
                assert_eq!(
                    format!("{barrier:?}"),
                    "Barrier { n: 2, arrived: 1, generation: 0, .. }"
                );
            })
        },
        None,
    );
}

/// The barrier introduces no randomness of its own, so every schedule replays: each iteration runs
/// a random schedule and then replays it, and fails if the replay diverges.
#[test]
fn no_uncontrolled_nondeterminism() {
    check_uncontrolled_nondeterminism(
        || {
            future::block_on(async {
                assert_eq!(rendezvous(3).await, 1);
                assert_eq!(rendezvous(2).await, 1);
            })
        },
        500,
    );
}

/// Two runs from the same seed take the same schedules, so they observe the same order of events.
#[test]
fn same_seed_observes_the_same_order() {
    fn run() -> Vec<usize> {
        let order = Arc::new(Mutex::new(Vec::new()));
        let seen = order.clone();
        check_random_with_seed(
            move || {
                let seen = seen.clone();
                future::block_on(async move {
                    let barrier = Arc::new(Barrier::new(3));
                    // Each task records its number in the order the tasks get past the barrier.
                    let tasks = (0..3)
                        .map(|id| {
                            let (barrier, seen) = (barrier.clone(), seen.clone());
                            future::spawn(async move {
                                barrier.wait().await;
                                seen.lock().unwrap().push(id);
                            })
                        })
                        .collect::<Vec<_>>();
                    for task in tasks {
                        task.await.unwrap();
                    }
                })
            },
            0xdecaf,
            50,
        );
        Arc::try_unwrap(order).unwrap().into_inner().unwrap()
    }

    let first = run();
    assert_eq!(first.len(), 3 * 50);
    assert_eq!(first, run());
}

/// A failure found from a seed is found again, the same way, from the same seed.
#[test]
fn same_seed_finds_the_same_failure() {
    fn run() -> String {
        let payload = panic::catch_unwind(AssertUnwindSafe(|| {
            check_random_with_seed(
                || {
                    future::block_on(async {
                        let barrier = Arc::new(Barrier::new(2));
                        let order = Arc::new(Mutex::new(Vec::new()));
                        let tasks = (0..2)
                            .map(|id| {
                                let (barrier, order) = (barrier.clone(), order.clone());
                                future::spawn(async move {
                                    let leader = barrier.wait().await.is_leader();
                                    order.lock().unwrap().push((id, leader));
                                })
                            })
                            .collect::<Vec<_>>();
                        for task in tasks {
                            task.await.unwrap();
                        }
                        let order = order.lock().unwrap();
                        // BUG (in this test program): task 0 is not always the leader.
                        assert!(order.contains(&(0, true)), "the leader was not task 0: {order:?}");
                    })
                },
                0x5eed,
                100,
            )
        }))
        .expect_err("some schedule makes task 1 the leader");
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap()
    }

    let first = run();
    assert!(first.contains("the leader was not task 0"), "{first}");
    assert_eq!(first, run());
}
