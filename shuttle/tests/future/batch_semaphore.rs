use crate::basic::clocks::{check_clock, me};
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use shuttle::future::{self, batch_semaphore::*};
use shuttle::{check_dfs, check_random, current, thread};
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use test_log::test;

/// The entry for `task` of the current task's clock: how many events of `task` the current task
/// is after (0 if its clock does not know `task` yet).
fn clock_entry(task: usize) -> usize {
    let clock = current::clock();
    let times: &[u32] = &clock;
    times.get(task).copied().unwrap_or(0) as usize
}

#[test]
fn batch_semaphore_basic() {
    check_dfs(
        || {
            let s = BatchSemaphore::new(3, Fairness::StrictlyFair);

            future::spawn(async move {
                s.acquire(2).await.unwrap();
                s.acquire(1).await.unwrap();
                let r = s.try_acquire(1);
                assert_eq!(r, Err(TryAcquireError::NoPermits));
                s.release(1);
                s.acquire(1).await.unwrap();
            });
        },
        None,
    );
}

/// Checks the behavior of an unfair batch semaphore is unfair: if there are
/// two threads blocked on the same semaphore, releasing permits may unblock
/// them in any order.
#[test]
fn batch_semaphore_unfair() {
    let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
    let observed_values_clone = Arc::clone(&observed_values);

    check_random(
        move || {
            let semaphore = Arc::new(BatchSemaphore::new(0, Fairness::Unfair));

            // Here we use a stdlib mutex to avoid introducing yield points.
            // It is used to record in which order the threads were enqueued
            // into the semaphore's waiters list, because `thread::spawn` is
            // a yield point and as such the enqueing can happen in either
            // order.
            let order1 = Arc::new(std::sync::Mutex::new(vec![]));
            let order2 = Arc::new(std::sync::Mutex::new(vec![]));
            let threads = (0..3)
                .map(|tid| {
                    let semaphore = semaphore.clone();
                    let order1 = order1.clone();
                    let order2 = order2.clone();
                    thread::spawn(move || {
                        // once the ID is pushed to the vector here and
                        // observed in the busy loop below, the thread is
                        // assumed to be blocked, because there is no yield
                        // point between the push and the acquire
                        order1.lock().unwrap().push(tid); // stdlib mutex
                        let val = [2, 1, 1][tid];
                        semaphore.acquire_blocking(val).unwrap(); // shuttle semaphore

                        // after unblock, record which thread acquired how many
                        order2.lock().unwrap().push((tid, val)); // stdlib mutex
                    })
                })
                .collect::<Vec<_>>();

            // wait until all threads are blocked on the semaphore
            while order1.lock().unwrap().len() < 3 {
                thread::yield_now();
            }

            // record the order in which they enqueued for the semaphore
            let order1_after_enqueued = order1.lock().unwrap().clone();

            // release 2 permits, which either unblocks thread 0 (which needs
            // 2 permits), or both thread 1 and 2 (both of which need 1 permit)
            semaphore.release(2);

            // wait until the threads unblock and finish
            while order2.lock().unwrap().iter().map(|(_tid, val)| val).sum::<usize>() < 2 {
                thread::yield_now();
            }

            // record the order in which they were woken
            let order2_after_release = order2.lock().unwrap().clone();

            // clean up: release 2 more permits to unblock any remaining
            // threads, then join all threads
            semaphore.release(2);
            for thread in threads {
                thread.join().unwrap();
            }

            observed_values_clone
                .lock()
                .unwrap()
                .insert((order1_after_enqueued, order2_after_release));
        },
        1000, // should be enough to find all permutations
    );

    // We expect to see 18 (= 6 * 3) different outcomes:
    // - the three threads may block on the semaphore in any order (6),
    // - once the permits are released, then either both are consumed by
    //   thread 0, or they are consumed threads 1 and 2 (3).
    let observed_values = Arc::try_unwrap(observed_values).unwrap().into_inner().unwrap();
    assert_eq!(
        observed_values,
        HashSet::from([
            (vec![0, 1, 2], vec![(0, 2)]),
            (vec![0, 1, 2], vec![(1, 1), (2, 1)]),
            (vec![0, 1, 2], vec![(2, 1), (1, 1)]),
            (vec![0, 2, 1], vec![(0, 2)]),
            (vec![0, 2, 1], vec![(1, 1), (2, 1)]),
            (vec![0, 2, 1], vec![(2, 1), (1, 1)]),
            (vec![1, 0, 2], vec![(0, 2)]),
            (vec![1, 0, 2], vec![(1, 1), (2, 1)]),
            (vec![1, 0, 2], vec![(2, 1), (1, 1)]),
            (vec![1, 2, 0], vec![(0, 2)]),
            (vec![1, 2, 0], vec![(1, 1), (2, 1)]),
            (vec![1, 2, 0], vec![(2, 1), (1, 1)]),
            (vec![2, 1, 0], vec![(0, 2)]),
            (vec![2, 1, 0], vec![(1, 1), (2, 1)]),
            (vec![2, 1, 0], vec![(2, 1), (1, 1)]),
            (vec![2, 0, 1], vec![(0, 2)]),
            (vec![2, 0, 1], vec![(1, 1), (2, 1)]),
            (vec![2, 0, 1], vec![(2, 1), (1, 1)]),
        ])
    );
}

#[test]
fn batch_semaphore_clock_1() {
    for fairness in [Fairness::StrictlyFair, Fairness::Unfair, Fairness::ParkingLot] {
        check_dfs(
            move || {
                let s = Arc::new(BatchSemaphore::new(0, fairness));

                let s2 = s.clone();
                thread::spawn(move || {
                    assert_eq!(me(), 1);
                    s2.release(1);
                });
                thread::spawn(move || {
                    assert_eq!(me(), 2);
                    check_clock(|i, c| (i != 1) || (c == 0));
                    s.acquire_blocking(1).unwrap();
                    // after the acquire, we are causally dependent on task 1
                    check_clock(|i, c| (i != 1) || (c > 0));
                });
            },
            None,
        );
    }
}

#[test]
fn batch_semaphore_clock_2() {
    for fairness in [Fairness::StrictlyFair, Fairness::Unfair, Fairness::ParkingLot] {
        check_dfs(
            move || {
                let s = Arc::new(BatchSemaphore::new(0, fairness));

                for i in 1..=2 {
                    let s2 = s.clone();
                    thread::spawn(move || {
                        assert_eq!(me(), i);
                        s2.release(1);
                    });
                }

                thread::spawn(move || {
                    assert_eq!(me(), 3);
                    check_clock(|i, c| (c > 0) == (i == 0));
                    // acquire 2: unblocked once both of the threads finished
                    s.acquire_blocking(2).unwrap();
                    // after the acquire, we are causally dependent on both tasks
                    check_clock(|i, c| (i == 3) || (c > 0));
                });
            },
            None,
        );
    }
}

#[test]
fn batch_semaphore_clock_3() {
    for fairness in [Fairness::StrictlyFair, Fairness::Unfair, Fairness::ParkingLot] {
        check_dfs(
            move || {
                let s = Arc::new(BatchSemaphore::new(0, fairness));

                for i in 1..=2 {
                    let s2 = s.clone();
                    thread::spawn(move || {
                        assert_eq!(me(), i);
                        s2.release(1);
                    });
                }

                thread::spawn(move || {
                    assert_eq!(me(), 3);
                    check_clock(|i, c| (c > 0) == (i == 0));
                    // acquire 1: unblocked once either of the threads finished
                    s.acquire_blocking(1).unwrap();
                    // after the acquire, we are causally dependent on exactly one of the two tasks
                    let clock = current::clock();
                    assert!((clock[1] > 0 && clock[2] == 0) || (clock[1] == 0 && clock[2] > 0));
                });
            },
            None,
        );
    }
}

#[test]
fn batch_semaphore_clock_4() {
    for fairness in [Fairness::StrictlyFair, Fairness::Unfair, Fairness::ParkingLot] {
        check_dfs(
            move || {
                let s = Arc::new(BatchSemaphore::new(1, fairness));

                for tid in 1..=2 {
                    let other_tid = 2 - tid;
                    let s2 = s.clone();
                    thread::spawn(move || {
                        assert_eq!(me(), tid);
                        match s2.try_acquire(1) {
                            Ok(()) => {
                                // we won the race, no causal dependence on another thread
                                check_clock(|i, c| (c > 0) == (i == 0 || i == tid));
                            }
                            Err(TryAcquireError::NoPermits) => {
                                // we lost the race, so we causally depend on the other thread
                                check_clock(|i, c| !(i == 0 || i == other_tid) || (c > 0));
                            }
                            Err(TryAcquireError::Closed) => unreachable!(),
                        }
                    });
                }
            },
            None,
        );
    }
}

/// A `try_upgrade` that too few free permits refuse is after the acquire of the task that holds
/// them, as a `try_acquire` that loses the race is (see `batch_semaphore_clock_4`). Here the main
/// task holds one permit of three and tries to upgrade to all three, while task 1 takes one and
/// gives it back. Task 1 releases another semaphore first, so that its clock has an event of its
/// own when it acquires.
#[test]
fn batch_semaphore_clock_try_upgrade() {
    use std::sync::atomic::AtomicBool;
    static REFUSED: AtomicBool = AtomicBool::new(false);
    for fairness in [Fairness::StrictlyFair, Fairness::Unfair, Fairness::ParkingLot] {
        REFUSED.store(false, Ordering::SeqCst);
        check_dfs(
            move || {
                let s = Arc::new(BatchSemaphore::new(3, fairness));
                s.acquire_blocking(1).unwrap();
                let holder = {
                    let s = s.clone();
                    thread::spawn(move || {
                        assert_eq!(me(), 1);
                        BatchSemaphore::new(0, Fairness::Unfair).release(1);
                        s.acquire_blocking(1).unwrap();
                        s.release(1);
                    })
                };
                check_clock(|i, c| (i != 1) || (c == 0));
                match s.try_upgrade(1, 3) {
                    Ok(()) => s.release(3),
                    Err(_) => {
                        REFUSED.store(true, Ordering::SeqCst);
                        // only task 1 can hold the permit that is missing
                        check_clock(|i, c| (i != 1) || (c > 0));
                        s.release(1);
                    }
                }
                holder.join().unwrap();
            },
            None,
        );
        assert!(
            REFUSED.load(Ordering::SeqCst),
            "{fairness:?}: no schedule refused the upgrade"
        );
    }
}

/// Shows a case in which causality tracking in the batch semaphore is
/// currently imprecise. The test sets up a semaphore with two permits and two
/// threads, each of which acquires one permit, then releases one permit, then
/// repeats. Neither thread can be blocked in this situation, and so neither
/// thread should causally depend on the other, but currently they do.
#[test]
#[should_panic(expected = "doesn't satisfy predicate")]
fn batch_semaphore_clock_imprecise() {
    check_dfs(
        move || {
            let s = Arc::new(BatchSemaphore::new(2, Fairness::StrictlyFair));

            for tid in 1..=2 {
                let s2 = s.clone();
                thread::spawn(move || {
                    assert_eq!(me(), tid);
                    for _ in 0..2 {
                        s2.try_acquire(1).unwrap();
                        s2.release(1);
                    }

                    // With precise causality tracking, this predicate should
                    // hold: each thread should only be aware of the events of
                    // its parent and its own usage of the semaphore.
                    check_clock(|i, c| (c > 0) == (i == 0 || i == tid));
                });
            }
        },
        None,
    );
}

// Create a semaphore with `num_permits` permits and have a bunch of tasks each try to grab a bunch
// of permits. Task i sets the i'th bit in a shared atomic counter while holding its permits, and
// records the counter value it observed *before* setting its own bit — that is, the set of tasks
// that were holding permits at the same time as it. Over a full DFS run this yields the exact set
// of possible co-residencies, which must be exactly those whose permit demands sum to at most
// `num_permits`.
//
// Note that the *last* participant runs on the calling task rather than being spawned. The calling
// task has to exist either way and performs no semaphore operations of its own, so giving every
// participant its own spawned task just adds a schedulable entity that multiplies the interleaving
// space without adding any contention. Folding the last participant into the caller keeps the same
// number of concurrent contenders and produces an identical set of observed states, while cutting
// the exhaustive search roughly 100x: for `(5, [3, 3, 2])` the DFS explores 15,376 interleavings
// instead of 1,554,091, and for `(5, [3, 3, 3])` it explores 4,437 instead of 590,311.
//
// Note also that `future::yield_now` below is load-bearing: it is the window during which a task's
// bit is observable to others. Without it the search is 22x cheaper but every task observes 0, so
// no co-residency is detected at all.
async fn semtest(num_permits: usize, counts: Vec<usize>, states: &Arc<Mutex<HashSet<(usize, usize)>>>, mode: Fairness) {
    // One participant: acquire `c` permits, publish bit `i` for one scheduling step, then release.
    async fn participant(
        i: usize,
        c: usize,
        s: Arc<BatchSemaphore>,
        r: Arc<AtomicUsize>,
        states: Arc<Mutex<HashSet<(usize, usize)>>>,
    ) {
        let val = 1usize << i;
        s.acquire(c).await.unwrap();
        let v = r.fetch_add(val, Ordering::SeqCst);
        future::yield_now().await;
        let _ = r.fetch_sub(val, Ordering::SeqCst);
        states.lock().unwrap().insert((i, v));
        s.release(c);
    }

    let s = Arc::new(BatchSemaphore::new(num_permits, mode));
    let r = Arc::new(AtomicUsize::new(0));

    let (&last, rest) = counts.split_last().expect("need at least one participant");
    let handles = rest
        .iter()
        .enumerate()
        .map(|(i, &c)| future::spawn(participant(i, c, s.clone(), r.clone(), states.clone())))
        .collect::<Vec<_>>();

    participant(counts.len() - 1, last, s.clone(), r.clone(), states.clone()).await;

    for h in handles {
        h.await.unwrap();
    }
}

#[test]
fn batch_semaphore_test_1() {
    let states = Arc::new(Mutex::new(HashSet::new()));
    let states2 = states.clone();
    check_dfs(
        move || {
            let states2 = states2.clone();
            future::block_on(async move {
                semtest(5, vec![3, 3, 3], &states2, Fairness::StrictlyFair).await;
            });
        },
        None,
    );

    let states = Arc::try_unwrap(states).unwrap().into_inner().unwrap();
    assert_eq!(states, HashSet::from([(0, 0), (1, 0), (2, 0)]));
}

#[test]
fn batch_semaphore_test_2() {
    let states = Arc::new(Mutex::new(HashSet::new()));
    let states2 = states.clone();
    check_dfs(
        move || {
            let states2 = states2.clone();
            future::block_on(async move {
                semtest(5, vec![3, 3, 2], &states2, Fairness::StrictlyFair).await;
            });
        },
        None,
    );

    let states = Arc::try_unwrap(states).unwrap().into_inner().unwrap();
    assert_eq!(
        states,
        HashSet::from([(0, 0), (1, 0), (2, 0), (0, 4), (1, 4), (2, 1), (2, 2)])
    );
}

#[test]
fn batch_semaphore_signal() {
    // Use a semaphore for signaling
    check_dfs(
        move || {
            let sem = Arc::new(BatchSemaphore::new(0, Fairness::StrictlyFair));
            let sem2 = sem.clone();
            let r = Arc::new(AtomicUsize::new(0));
            let r2 = r.clone();
            future::spawn(async move {
                sem.acquire(1).await.unwrap();
                let v = r2.load(Ordering::SeqCst);
                assert!(v > 0);
                sem.acquire(1).await.unwrap();
                let v = r2.load(Ordering::SeqCst);
                assert!(v > 1);
            });
            r.store(1, Ordering::SeqCst);
            sem2.release(1);
            r.store(2, Ordering::SeqCst);
            sem2.release(1);
        },
        None,
    );
}

/// A queued `Acquire` that is polled by a second task must wake *that* task.
///
/// An `Acquire` is a future like any other: it can be created and first polled
/// by one task, stored in a longer-lived object, and later polled by a different
/// task. Tokio's `Receiver::poll_recv(&mut self, cx)` is the motivating example
/// — it keeps its in-flight acquire inside the `Receiver`, so a `Receiver` that
/// is moved between tasks carries the acquire with it.
///
/// The waiter therefore has to follow the poller, both its waker and the task it
/// unblocks. If it keeps pointing at whoever polled first, a later `release`
/// hands the permits to a task that is no longer waiting and never notifies the
/// task that actually is.
///
/// The two pollers here use their own wakers so the test can assert *which* one
/// the semaphore notified. Asserting on the wakers rather than on progress is
/// deliberate: a lost wakeup does not reliably deadlock, because a `Pending`
/// future is left sleeping and Shuttle may schedule a spurious re-poll that
/// papers over the missing notification.
///
/// The same holds on a ParkingLot semaphore, whose release wakes the waiter to
/// try again rather than grant it the permit.
#[test]
fn queued_acquire_polled_by_second_task_is_woken() {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::AtomicBool;
    use std::task::Context;

    shuttle::lazy_static! {
        static ref SEM: BatchSemaphore = BatchSemaphore::new(0, Fairness::StrictlyFair);
        // Parks the first task so it stays alive (and thus not "stale") while
        // the second task waits on the acquire it registered.
        static ref PARK: BatchSemaphore = BatchSemaphore::new(0, Fairness::StrictlyFair);
        static ref PARKING_LOT_SEM: BatchSemaphore = BatchSemaphore::new(0, Fairness::ParkingLot);
        static ref PARKING_LOT_PARK: BatchSemaphore = BatchSemaphore::new(0, Fairness::ParkingLot);
    }

    /// Records whether it was woken, so the test can tell *which* poller the
    /// semaphore notified.
    #[derive(Debug, Default)]
    struct Recorder(AtomicBool);

    impl Recorder {
        fn woken(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    impl futures::task::ArcWake for Recorder {
        fn wake_by_ref(arc_self: &Arc<Self>) {
            arc_self.0.store(true, Ordering::SeqCst);
        }
    }

    /// Polls `acquire` once with a waker belonging to `recorder`, asserting it
    /// does not resolve, so the waiter stays queued.
    fn poll_pending(acquire: &mut Pin<Box<Acquire<'static>>>, recorder: &Arc<Recorder>) {
        let waker = futures::task::waker(recorder.clone());
        let mut cx = Context::from_waker(&waker);
        assert!(acquire.as_mut().poll(&mut cx).is_pending());
    }

    for parking_lot in [false, true] {
        check_dfs(
            move || {
                let (sem, park): (&'static BatchSemaphore, &'static BatchSemaphore) = if parking_lot {
                    (&PARKING_LOT_SEM, &PARKING_LOT_PARK)
                } else {
                    (&SEM, &PARK)
                };
                future::block_on(async move {
                    // The `Acquire` outlives the task that first polls it. A stdlib
                    // mutex keeps the handoff free of extra yield points.
                    let acquire = Arc::new(Mutex::new(Some(Box::pin(sem.acquire(1)))));
                    let recorder_a = Arc::new(Recorder::default());
                    let recorder_b = Arc::new(Recorder::default());
                    let (polled_tx, polled_rx) = shuttle::sync::mpsc::channel();

                    let mut tasks = vec![];
                    for recorder in [recorder_a.clone(), recorder_b.clone()] {
                        let acquire = acquire.clone();
                        let polled_tx = polled_tx.clone();
                        tasks.push(future::spawn(async move {
                            let mut acq = acquire.lock().unwrap().take().unwrap();
                            // No permits are available, so this leaves a waiter
                            // queued, pointing at this task and this waker.
                            poll_pending(&mut acq, &recorder);
                            // Hand the still-queued acquire on, then park without
                            // ever polling it again.
                            *acquire.lock().unwrap() = Some(acq);
                            polled_tx.send(()).unwrap();
                            park.acquire(1).await.unwrap();
                        }));
                        // Serialize the two polls: the second task must be the last
                        // one to have polled when the release below happens.
                        polled_rx.recv().unwrap();
                    }

                    // Grants the permit to the queued waiter (on a ParkingLot
                    // semaphore, wakes it to try again), which must notify the task
                    // that polled most recently rather than the one that first
                    // registered.
                    sem.release(1);
                    assert!(recorder_b.woken(), "second poller was not woken");
                    assert!(!recorder_a.woken(), "first poller was woken instead");

                    park.release(2);
                    for task in tasks {
                        task.await.unwrap();
                    }
                });
            },
            // The handshake above pins down the ordering the assertions depend on,
            // so the remaining interleavings are incidental; a bounded search keeps
            // this cheap.
            Some(500),
        );
    }
}

/// The companion case to `queued_acquire_polled_by_second_task_is_woken` in
/// which the task that queued the waiter finishes once the second task has
/// polled the `Acquire`. The waiter must follow the second task, so that a
/// release does not take it for a waiter of a finished task (see
/// `release_to_waiter_of_finished_task`), and drop it, while the second task
/// waits for it.
#[test]
fn queued_acquire_polled_by_second_task_outlives_its_first_task() {
    use std::pin::Pin;
    type Stash = Arc<Mutex<Option<Pin<Box<Acquire<'static>>>>>>;

    shuttle::lazy_static! {
        static ref STRICTLY_FAIR: BatchSemaphore = BatchSemaphore::new(1, Fairness::StrictlyFair);
        static ref UNFAIR: BatchSemaphore = BatchSemaphore::new(1, Fairness::Unfair);
        static ref PARKING_LOT: BatchSemaphore = BatchSemaphore::new(1, Fairness::ParkingLot);
    }

    for fairness in [Fairness::StrictlyFair, Fairness::Unfair, Fairness::ParkingLot] {
        check_dfs(
            move || {
                let sem: &'static BatchSemaphore = match fairness {
                    Fairness::StrictlyFair => &STRICTLY_FAIR,
                    Fairness::Unfair => &UNFAIR,
                    _ => &PARKING_LOT,
                };
                sem.try_acquire(1).unwrap();
                // A stdlib mutex keeps the handoff free of extra yield points.
                let stash: Stash = Arc::new(Mutex::new(None));
                let (queued_tx, queued_rx) = shuttle::sync::mpsc::channel::<()>();
                let (polled_tx, polled_rx) = shuttle::sync::mpsc::channel::<()>();
                let first = {
                    let stash = Arc::clone(&stash);
                    thread::spawn(move || {
                        future::block_on(async {
                            let mut acquire = Box::pin(sem.acquire(1));
                            // No permit is free, so this queues a waiter for this task.
                            assert!(futures::poll!(acquire.as_mut()).is_pending());
                            *stash.lock().unwrap() = Some(acquire);
                        });
                        queued_tx.send(()).unwrap();
                        polled_rx.recv().unwrap();
                    })
                };
                let second = {
                    let stash = Arc::clone(&stash);
                    thread::spawn(move || {
                        queued_rx.recv().unwrap();
                        let mut acquire = stash.lock().unwrap().take().unwrap();
                        future::block_on(async {
                            // Still queued, now for this task.
                            assert!(futures::poll!(acquire.as_mut()).is_pending());
                            polled_tx.send(()).unwrap();
                            acquire.await.unwrap();
                        });
                        sem.release(1);
                    })
                };
                first.join().unwrap(); // the task that queued the waiter has finished
                sem.release(1);
                second.join().unwrap();
            },
            None,
        );
    }
}

/// Releasing permits to a waiter whose task has finished must not panic, and
/// must not lose the permits.
///
/// The companion case to `queued_acquire_polled_by_second_task_is_woken`: here
/// the task that registered the waiter exits without ever resolving or dropping
/// its `Acquire`, because the `Acquire` was stored somewhere that outlives the
/// task. Nobody is waiting on that waiter, so a later `release` has nobody to
/// unblock — it must discard the waiter rather than assert that the registering
/// task is still alive.
///
/// The permits must stay available: the abandoned `Acquire` is still a live
/// future, and polling it from a task that *is* running has to succeed. On a
/// ParkingLot semaphore, the waiter is still parked when the release passes it
/// over, and a parked waiter does not try again until a release wakes it, so
/// the poll of the live task must take it out of the queue.
#[test]
fn release_to_waiter_of_finished_task() {
    // A `lazy_static` semaphore gives the `Acquire` a `'static` borrow, so it can
    // be handed to a `'static` spawned task and outlive it.
    shuttle::lazy_static! {
        static ref STRICTLY_FAIR: BatchSemaphore = BatchSemaphore::new(0, Fairness::StrictlyFair);
        static ref UNFAIR: BatchSemaphore = BatchSemaphore::new(0, Fairness::Unfair);
        static ref PARKING_LOT: BatchSemaphore = BatchSemaphore::new(0, Fairness::ParkingLot);
    }

    for fairness in [Fairness::StrictlyFair, Fairness::Unfair, Fairness::ParkingLot] {
        check_dfs(
            move || {
                let sem: &'static BatchSemaphore = match fairness {
                    Fairness::StrictlyFair => &STRICTLY_FAIR,
                    Fairness::Unfair => &UNFAIR,
                    _ => &PARKING_LOT,
                };
                future::block_on(async move {
                    // A stdlib mutex keeps the handoff free of extra yield points.
                    let acquire = Arc::new(Mutex::new(None));

                    let acquire2 = Arc::clone(&acquire);
                    let registrant = future::spawn(async move {
                        // Created *and* first polled by this task, so the queued
                        // waiter belongs to the task that is about to finish.
                        let mut acq = Box::pin(sem.acquire(1));
                        // No permits are available, so this leaves a waiter queued.
                        assert!(futures::poll!(acq.as_mut()).is_pending());
                        // Hand the still-queued acquire back out so it outlives this
                        // task, then finish without polling or dropping it again.
                        *acquire2.lock().unwrap() = Some(acq);
                    });
                    registrant.await.unwrap();

                    // The only queued waiter belongs to a task that has finished.
                    sem.release(1);
                    assert_eq!(
                        sem.available_permits(),
                        1,
                        "permits were consumed by a waiter nobody is waiting on"
                    );

                    // The abandoned acquire is still usable by a live task.
                    let acq = acquire.lock().unwrap().take().unwrap();
                    acq.await.unwrap();
                    assert_eq!(sem.available_permits(), 0);
                });
            },
            None,
        );
    }
}

/// As `release_to_waiter_of_finished_task`, for a reserving acquire (see
/// `BatchSemaphore::acquire_reserving`) that waits behind a request that holds
/// all of the permits, like a writer of an `RwLock` behind another writer.
#[test]
fn release_to_reserving_waiter_of_finished_task() {
    shuttle::lazy_static! {
        static ref UNFAIR: BatchSemaphore = BatchSemaphore::new(2, Fairness::Unfair);
        static ref PARKING_LOT: BatchSemaphore = BatchSemaphore::new(2, Fairness::ParkingLot);
    }

    for fairness in [Fairness::Unfair, Fairness::ParkingLot] {
        check_dfs(
            move || {
                let sem: &'static BatchSemaphore = match fairness {
                    Fairness::Unfair => &UNFAIR,
                    _ => &PARKING_LOT,
                };
                future::block_on(async move {
                    sem.acquire(2).await.unwrap();
                    let acquire = Arc::new(Mutex::new(None));
                    let registrant = future::spawn({
                        let acquire = Arc::clone(&acquire);
                        async move {
                            let mut acq = Box::pin(sem.acquire_reserving(1, 2));
                            assert!(futures::poll!(acq.as_mut()).is_pending());
                            *acquire.lock().unwrap() = Some(acq);
                        }
                    });
                    registrant.await.unwrap();

                    sem.release(2);
                    let acq = acquire.lock().unwrap().take().unwrap();
                    acq.await.unwrap();
                    assert_eq!(sem.available_permits(), 0);
                    sem.release(2);
                });
            },
            None,
        );
    }
}

/// As `release_to_waiter_of_finished_task`, with the waiter of the finished task
/// queued behind a live one. The release lets only the live waiter in, whose
/// `Acquire` is then dropped. On a ParkingLot semaphore, that passes the wake on
/// (see `BatchSemaphore::release`), which finds only the waiter of the finished
/// task in the queue. A live task that polls its abandoned `Acquire` must then
/// get the free permit.
#[test]
fn waiter_of_finished_task_behind_a_dropped_acquire() {
    shuttle::lazy_static! {
        static ref STRICTLY_FAIR: BatchSemaphore = BatchSemaphore::new(0, Fairness::StrictlyFair);
        static ref UNFAIR: BatchSemaphore = BatchSemaphore::new(0, Fairness::Unfair);
        static ref PARKING_LOT: BatchSemaphore = BatchSemaphore::new(0, Fairness::ParkingLot);
    }

    for fairness in [Fairness::StrictlyFair, Fairness::Unfair, Fairness::ParkingLot] {
        check_dfs(
            move || {
                let sem: &'static BatchSemaphore = match fairness {
                    Fairness::StrictlyFair => &STRICTLY_FAIR,
                    Fairness::Unfair => &UNFAIR,
                    _ => &PARKING_LOT,
                };
                future::block_on(async move {
                    let mut live = Box::pin(sem.acquire(1));
                    assert!(futures::poll!(live.as_mut()).is_pending());
                    let acquire = Arc::new(Mutex::new(None));
                    let registrant = future::spawn({
                        let acquire = Arc::clone(&acquire);
                        async move {
                            let mut acq = Box::pin(sem.acquire(1));
                            assert!(futures::poll!(acq.as_mut()).is_pending());
                            *acquire.lock().unwrap() = Some(acq);
                        }
                    });
                    registrant.await.unwrap();

                    sem.release(1); // grants or wakes `live`
                    drop(live); // releases its permit, or passes its wake on
                    let acq = acquire.lock().unwrap().take().unwrap();
                    acq.await.unwrap();
                    sem.release(1);
                });
            },
            None,
        );
    }
}

#[test]
fn batch_semaphore_close_acquire() {
    // Check that closing a semaphore is handled gracefully
    check_dfs(
        || {
            future::block_on(async {
                let tx = Arc::new(BatchSemaphore::new(1, Fairness::StrictlyFair));
                let rx = Arc::new(BatchSemaphore::new(0, Fairness::StrictlyFair));
                let tx2 = tx.clone();
                let rx2 = rx.clone();

                let h = future::spawn(async move {
                    tx2.acquire(1).await.unwrap();
                    rx2.release(1);
                    let s = tx2.acquire(1).await;
                    assert!(s.is_err());
                    assert!(matches!(tx2.try_acquire(1), Err(TryAcquireError::Closed)));
                });

                rx.acquire(1).await.unwrap();
                tx.close();
                h.await.unwrap();
            });
        },
        None,
    );
}

#[test]
fn batch_semaphore_drop_sender() {
    struct Sender {
        sem: Arc<BatchSemaphore>,
    }

    impl Drop for Sender {
        fn drop(&mut self) {
            self.sem.close();
        }
    }

    // Check that closing a semaphore is handled gracefully
    check_dfs(
        || {
            future::block_on(async {
                let sem = Arc::new(BatchSemaphore::new(0, Fairness::StrictlyFair));
                let sender = Sender { sem: sem.clone() };

                future::spawn(async move {
                    let r = sem.acquire(2).await;
                    assert!(r.is_err());
                });

                future::spawn(async move {
                    sender.sem.release(1);
                    // sender is dropped here which will cause the semaphore to be closed
                });
            });
        },
        None,
    );
}

// Created to catch an issue where we would dequeue the wrong waiter from the `BatchSemaphore`.
//
// The idea of the test is to hit the following (or equivalent):
//
// 1. `TaskId(0)` (main thread) `lock`s and gets the `Guard`.
// 2. `handle.await.unwrap()`. We wait for the following to happen:
// 3. `lock_future1` (`TaskId(1)`) tries to `lock` and gets enequeued on the semaphore.
// 4. `lock_future2` (`TaskId(1)`) tries to `lock` and gets enequeued on the semaphore.
// 5. `empty_future` gets hit, and we return.
//
// At this point everything inside `handle` block will get dropped.
// This should result in the dequeueing of the waiters for `lock_future1` and `lock_future2`.
// What instead would happen is the following:
// - `lock_future2` gets dropped, and `lock_future1` gets dequeued erroneously.
// - `lock_future1` gets dropped, resulting in a no-op, as it is not enqueued.
// This means that the permit is held by `TaskId(0)`, and `lock_future2`(`TaskId(1)`) enqueued.
//
// 6. `guard` is dropped by `TaskId(0)`
// This releases the permit, giving it to `lock_future2`(`TaskId(1)`) erroneously.
// 7. `TaskId(0)` (main thread) tries to `lock` and gets enqueued on the semaphore.
// 8. Deadlock.
#[test]
fn bugged_cleanup_would_cause_deadlock() {
    struct Guard {
        sem: Arc<BatchSemaphore>,
    }

    async fn lock(sem: &Arc<BatchSemaphore>) -> Guard {
        let _ = sem.acquire(1).await;
        Guard { sem: sem.clone() }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            self.sem.release(1);
        }
    }

    check_dfs(
        || {
            let sem = Arc::new(BatchSemaphore::new(1, Fairness::StrictlyFair));
            let sem2 = sem.clone();

            future::block_on(async move {
                let handle = future::spawn(async move {
                    let mut futunord = FuturesUnordered::new();

                    let lock_future1 = async {
                        lock(&sem2).await;
                    }
                    .boxed();

                    let lock_future2 = async {
                        lock(&sem2).await;
                    }
                    .boxed();

                    let empty_future = async {}.boxed();

                    futunord.push(lock_future1);
                    futunord.push(lock_future2);
                    futunord.push(empty_future);

                    // Wait for any future to complete
                    futunord.next().await.unwrap();
                });

                let guard = lock(&sem).await;

                handle.await.unwrap();

                drop(guard);

                lock(&sem).await;
            });
        },
        None,
    )
}

/// A task that takes a permit of an unfair semaphore while an `Acquire` of its own is still queued
/// on it isn't blocked by that `Acquire`: the task is running.
#[test]
fn queued_acquire_of_the_current_task_does_not_block_it() {
    check_dfs(
        || {
            future::block_on(async {
                let sem = BatchSemaphore::new(0, Fairness::Unfair);
                let mut queued = Box::pin(sem.acquire(1));
                assert!(futures::poll!(queued.as_mut()).is_pending());
                sem.release(1);
                // Re-blocks the waiters that can no longer succeed, but not the task's own one.
                sem.try_acquire(1).unwrap();
                thread::yield_now();
                drop(queued);
                sem.release(1);
            });
        },
        None,
    );
}

/// Tests of `BatchSemaphore::acquire_reserving`, and of `upgrade` on an unfair or a ParkingLot
/// semaphore, which reserves the semaphore too. They run on both modes.
mod reservation_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// Whether an event of `task` happens before the current point of the current task.
    fn after(task: usize) -> bool {
        let clock = current::clock();
        let times: &[u32] = &clock;
        times.get(task).is_some_and(|&time| time > 0)
    }

    macro_rules! reservation_tests {
        ($mod_name:ident, $fairness:expr) => {
            mod $mod_name {
                use super::*;

                /// Until `min_permits` permits are available, a reserving acquire holds nothing and
                /// stops no other request. Here the main task holds 3 of 4 permits, so the
                /// reserver, which reserves at 2, waits, and the main task still gets the last
                /// permit.
                #[test_log::test]
                fn waits_without_reserving_below_min_permits() {
                    check_dfs(
                        || {
                            let sem = Arc::new(BatchSemaphore::new(4, $fairness));
                            sem.acquire_blocking(3).unwrap();
                            let reserver = {
                                let sem = sem.clone();
                                thread::spawn(move || {
                                    future::block_on(sem.acquire_reserving(2, 4)).unwrap();
                                    sem.release(4);
                                })
                            };
                            sem.acquire_blocking(1).unwrap();
                            sem.release(4);
                            reserver.join().unwrap();
                        },
                        None,
                    );
                }

                /// Once `min_permits` permits are available, the request reserves the semaphore: no
                /// other request can take a permit until the reservation is granted, although
                /// permits are free.
                #[test_log::test]
                fn reservation_keeps_permits_from_other_requests() {
                    check_dfs(
                        || {
                            future::block_on(async {
                                let sem = Arc::new(BatchSemaphore::new(4, $fairness));
                                // The main task holds one permit, like a reader of an `RwLock`.
                                sem.acquire(1).await.unwrap();
                                let mut reserve = Box::pin(sem.acquire_reserving(2, 4));
                                // 3 permits are available, which is at least 2, so this reserves
                                // the semaphore.
                                assert!(futures::poll!(reserve.as_mut()).is_pending());
                                assert_eq!(sem.available_permits(), 0);

                                let granted = Arc::new(AtomicBool::new(false));
                                let other = future::spawn({
                                    let (sem, granted) = (sem.clone(), granted.clone());
                                    async move {
                                        if sem.try_acquire(1).is_ok() {
                                            assert!(
                                                granted.load(Ordering::SeqCst),
                                                "try_acquire took a reserved permit"
                                            );
                                            sem.release(1);
                                        }
                                        sem.acquire(1).await.unwrap();
                                        assert!(granted.load(Ordering::SeqCst), "acquire took a reserved permit");
                                        sem.release(1);
                                    }
                                });

                                sem.release(1);
                                reserve.await.unwrap();
                                granted.store(true, Ordering::SeqCst);
                                sem.release(4);
                                other.await.unwrap();
                            });
                        },
                        None,
                    );
                }

                /// The reserver takes the permits that the holders release, so it is after each
                /// holder that released, and not after a task that was not let in before it.
                #[test_log::test]
                fn reserver_is_after_the_releases_it_waits_for() {
                    check_dfs(
                        || {
                            let sem = Arc::new(BatchSemaphore::new(4, $fairness));
                            let held = Arc::new(Mutex::new(Vec::new()));
                            let holders = (0..2)
                                .map(|_| {
                                    let (sem, held) = (sem.clone(), held.clone());
                                    thread::spawn(move || {
                                        sem.acquire_blocking(1).unwrap();
                                        held.lock().unwrap().push(me());
                                        sem.release(1);
                                    })
                                })
                                .collect::<Vec<_>>();
                            let holder_ids = holders
                                .iter()
                                .map(|holder| usize::from(holder.thread().id()))
                                .collect::<Vec<_>>();

                            future::block_on(sem.acquire_reserving(1, 4)).unwrap();
                            {
                                let held = held.lock().unwrap();
                                for id in holder_ids {
                                    assert_eq!(after(id), held.contains(&id), "holder {id}, holders {held:?}");
                                }
                            }
                            sem.release(4);
                            for holder in holders {
                                holder.join().unwrap();
                            }
                        },
                        None,
                    );
                }

                /// Only one request can hold the reservation. A second reserving request waits
                /// until the first is granted, although on its own (with `min_permits` 0) it would
                /// reserve at once.
                #[test_log::test]
                fn one_reservation_at_a_time() {
                    check_dfs(
                        || {
                            future::block_on(async {
                                let sem = Arc::new(BatchSemaphore::new(4, $fairness));
                                sem.acquire(1).await.unwrap();
                                let mut first = Box::pin(sem.acquire_reserving(1, 4));
                                assert!(futures::poll!(first.as_mut()).is_pending());

                                let granted = Arc::new(AtomicBool::new(false));
                                let second = future::spawn({
                                    let (sem, granted) = (sem.clone(), granted.clone());
                                    async move {
                                        sem.acquire_reserving(0, 1).await.unwrap();
                                        assert!(
                                            granted.load(Ordering::SeqCst),
                                            "a second reservation overtook the first"
                                        );
                                        sem.release(1);
                                    }
                                });

                                sem.release(1);
                                first.await.unwrap();
                                granted.store(true, Ordering::SeqCst);
                                sem.release(4);
                                second.await.unwrap();
                                assert_eq!(sem.available_permits(), 4);
                            });
                        },
                        None,
                    );
                }

                /// Dropping a reserving acquire that holds the reservation ends the reservation,
                /// and the permits it kept are available again, also to a task that already waits
                /// for them. The yield lets the other task wait behind the reservation before the
                /// drop, in some schedules.
                #[test_log::test]
                fn dropped_reservation_frees_the_permits() {
                    static WAITED: AtomicBool = AtomicBool::new(false);
                    check_dfs(
                        || {
                            future::block_on(async {
                                let sem = Arc::new(BatchSemaphore::new(2, $fairness));
                                sem.acquire(1).await.unwrap();
                                let mut reserve = Box::pin(sem.acquire_reserving(1, 2));
                                assert!(futures::poll!(reserve.as_mut()).is_pending());
                                let other = future::spawn({
                                    let sem = sem.clone();
                                    async move {
                                        sem.acquire(1).await.unwrap();
                                        sem.release(1);
                                    }
                                });
                                future::yield_now().await;
                                if sem.has_waiters() {
                                    WAITED.store(true, Ordering::SeqCst);
                                }
                                drop(reserve);
                                other.await.unwrap();
                                sem.release(1);
                                assert_eq!(sem.available_permits(), 2);
                            });
                        },
                        None,
                    );
                    assert!(
                        WAITED.load(Ordering::SeqCst),
                        "the other task never waited at the drop"
                    );
                }

                /// A release that drops the reservation of a finished task (see
                /// `reservation_of_finished_task_is_dropped`) ends it, so the requests that it
                /// kept out are let in, also by a release that otherwise wakes nobody in the queue
                /// (`release_without_waking`, the unlock of a reader of an `RwLock`): nothing else
                /// would wake them. Here the reservation's task finishes before or after the
                /// reader parks behind the reservation.
                #[test_log::test]
                fn dropped_reservation_of_finished_task_lets_the_waiters_in() {
                    shuttle::lazy_static! {
                        static ref SEM: BatchSemaphore = BatchSemaphore::new(2, $fairness);
                    }

                    for finishes_first in [true, false] {
                        check_dfs(
                            move || {
                                future::block_on(async move {
                                    SEM.acquire(1).await.unwrap(); // a reader holds the lock
                                    let acquire = Arc::new(Mutex::new(None));
                                    let (reserved_tx, reserved_rx) = shuttle::sync::mpsc::channel::<()>();
                                    let (finish_tx, finish_rx) = shuttle::sync::mpsc::channel::<()>();
                                    let mut registrant = Some(future::spawn({
                                        let acquire = acquire.clone();
                                        async move {
                                            let mut acq = Box::pin(SEM.acquire_reserving(1, 2));
                                            // One permit is free, so this reserves the semaphore.
                                            assert!(futures::poll!(acq.as_mut()).is_pending());
                                            *acquire.lock().unwrap() = Some(acq);
                                            reserved_tx.send(()).unwrap();
                                            finish_rx.recv().unwrap();
                                        }
                                    }));
                                    reserved_rx.recv().unwrap();
                                    if finishes_first {
                                        finish_tx.send(()).unwrap();
                                        registrant.take().unwrap().await.unwrap();
                                    }
                                    let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
                                    let reader = future::spawn(async move {
                                        let mut acq = Box::pin(SEM.acquire(1));
                                        // The reservation keeps the free permit.
                                        assert!(futures::poll!(acq.as_mut()).is_pending());
                                        parked_tx.send(()).unwrap();
                                        acq.await.unwrap();
                                        SEM.release_without_waking(1);
                                    });
                                    parked_rx.recv().unwrap();
                                    if let Some(registrant) = registrant {
                                        finish_tx.send(()).unwrap();
                                        registrant.await.unwrap();
                                    }
                                    SEM.release_without_waking(1); // drops the reservation
                                    reader.await.unwrap();
                                    drop(acquire.lock().unwrap().take());
                                    assert_eq!(SEM.available_permits(), 2);
                                });
                            },
                            None,
                        );
                    }
                }

                /// Closing the semaphore wakes the holder of the reservation, which then fails.
                #[test_log::test]
                fn close_wakes_the_reserver() {
                    check_dfs(
                        || {
                            future::block_on(async {
                                let sem = Arc::new(BatchSemaphore::new(2, $fairness));
                                sem.acquire(1).await.unwrap();
                                let reserver = future::spawn({
                                    let sem = sem.clone();
                                    async move { sem.acquire_reserving(1, 2).await }
                                });
                                sem.close();
                                assert!(reserver.await.unwrap().is_err());
                            });
                        },
                        None,
                    );
                }

                /// A reservation whose task finished without dropping its `Acquire` must not keep
                /// the permits from everyone else. A release drops it, and the abandoned `Acquire`
                /// still works when a live task polls it (see
                /// `release_to_waiter_of_finished_task`).
                #[test_log::test]
                fn reservation_of_finished_task_is_dropped() {
                    shuttle::lazy_static! {
                        static ref SEM: BatchSemaphore = BatchSemaphore::new(2, $fairness);
                    }

                    check_dfs(
                        || {
                            future::block_on(async {
                                SEM.acquire(1).await.unwrap();
                                let acquire = Arc::new(Mutex::new(None));
                                let registrant = future::spawn({
                                    let acquire = acquire.clone();
                                    async move {
                                        let mut acq = Box::pin(SEM.acquire_reserving(1, 2));
                                        assert!(futures::poll!(acq.as_mut()).is_pending());
                                        *acquire.lock().unwrap() = Some(acq);
                                    }
                                });
                                registrant.await.unwrap();

                                SEM.release(1);
                                assert_eq!(
                                    SEM.available_permits(),
                                    2,
                                    "the reservation of a finished task kept the permits"
                                );

                                let acq = acquire.lock().unwrap().take().unwrap();
                                acq.await.unwrap();
                                assert_eq!(SEM.available_permits(), 0);
                                SEM.release(2);
                            });
                        },
                        None,
                    );
                }

                /// An upgrade reserves the semaphore at once: new requests wait until the upgrade
                /// is granted, and the upgrade waits only for the permits that are held.
                #[test_log::test]
                fn upgrade_reserves_the_semaphore() {
                    check_dfs(
                        || {
                            future::block_on(async {
                                let sem = Arc::new(BatchSemaphore::new(4, $fairness));
                                // The upgrading task's permits, and one permit of a reader.
                                sem.acquire(2).await.unwrap();
                                sem.acquire(1).await.unwrap();
                                let mut upgrade = Box::pin(sem.upgrade(2, 4));
                                assert!(futures::poll!(upgrade.as_mut()).is_pending());
                                assert_eq!(sem.available_permits(), 0);

                                let granted = Arc::new(AtomicBool::new(false));
                                let other = future::spawn({
                                    let (sem, granted) = (sem.clone(), granted.clone());
                                    async move {
                                        sem.acquire(1).await.unwrap();
                                        assert!(granted.load(Ordering::SeqCst), "a request overtook the upgrade");
                                        sem.release(1);
                                    }
                                });

                                // The reader leaves.
                                sem.release(1);
                                upgrade.await.unwrap();
                                granted.store(true, Ordering::SeqCst);
                                sem.release(4);
                                other.await.unwrap();
                            });
                        },
                        None,
                    );
                }

                /// A request that will reserve the semaphore changes its state, so it gets a
                /// scheduling point before it reserves, like a request that will be granted.
                /// Without one, the reservation would happen in the same step as whatever the task
                /// did before it asked, and no other task could run in between. Here the reserver
                /// sets a flag, then asks while the main task holds a permit, so it reserves and
                /// waits. Some schedule must let another task see the flag and still take a permit,
                /// as a `parking_lot` reader can between a writer's earlier action and its
                /// `WRITER_BIT`.
                #[test_log::test]
                fn scheduling_point_before_reserving() {
                    static TOOK_PERMIT_AFTER_FLAG: AtomicBool = AtomicBool::new(false);
                    check_dfs(
                        || {
                            let sem = Arc::new(BatchSemaphore::new(4, $fairness));
                            sem.acquire_blocking(1).unwrap();
                            let flag = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
                            let reserver = {
                                let (sem, flag) = (sem.clone(), flag.clone());
                                thread::spawn(move || {
                                    flag.store(true, Ordering::SeqCst);
                                    // 3 permits are available, which is at least 2, so this
                                    // reserves.
                                    future::block_on(sem.acquire_reserving(2, 4)).unwrap();
                                    sem.release(4);
                                })
                            };
                            let other = {
                                let (sem, flag) = (sem.clone(), flag.clone());
                                thread::spawn(move || {
                                    if flag.load(Ordering::SeqCst) && sem.try_acquire(1).is_ok() {
                                        TOOK_PERMIT_AFTER_FLAG.store(true, Ordering::SeqCst);
                                        sem.release(1);
                                    }
                                })
                            };
                            other.join().unwrap();
                            sem.release(1);
                            reserver.join().unwrap();
                        },
                        None,
                    );
                    assert!(
                        TOOK_PERMIT_AFTER_FLAG.load(Ordering::SeqCst),
                        "no schedule let a task take a permit between the reserver's flag and its reservation"
                    );
                }

                /// A `try_acquire` that a reservation refuses is after the request that holds the
                /// reservation, although that request holds no permits yet, as a `try_write` of a
                /// `parking_lot` lock that sees `WRITER_BIT` is after the writer that set it. Task
                /// 1 releases another semaphore first, so that its clock has an event of its own
                /// when it reserves the semaphore.
                #[test_log::test]
                fn failed_try_acquire_is_after_the_reservation() {
                    check_dfs(
                        || {
                            let sem = Arc::new(BatchSemaphore::new(2, $fairness));
                            sem.acquire_blocking(1).unwrap();
                            let reserver = {
                                let sem = Arc::clone(&sem);
                                thread::spawn(move || {
                                    assert_eq!(me(), 1);
                                    BatchSemaphore::new(0, Fairness::Unfair).release(1);
                                    future::block_on(sem.acquire_reserving(1, 2)).unwrap();
                                    sem.release(2);
                                })
                            };
                            let other = {
                                let sem = Arc::clone(&sem);
                                thread::spawn(move || {
                                    assert_eq!(me(), 2);
                                    // One permit is free while the main task holds the other, so
                                    // only task 1 can make this fail, by reserving the semaphore or
                                    // by holding it.
                                    match sem.try_acquire(1) {
                                        Ok(()) => sem.release(1),
                                        Err(_) => check_clock(|i, c| (i != 1) || (c > 0)),
                                    }
                                })
                            };
                            thread::yield_now();
                            sem.release(1);
                            reserver.join().unwrap();
                            other.join().unwrap();
                        },
                        None,
                    );
                }

                /// The same for a `try_upgrade`. The main task holds two of the four permits, as
                /// an upgradable reader of an `RwLock` does, and task 1 reserves the semaphore
                /// after an event of its own, which the main task is not after. The two free
                /// permits let the upgrade in unless task 1 has reserved them first.
                #[test_log::test]
                fn failed_try_upgrade_is_after_the_reservation() {
                    static REFUSED: AtomicBool = AtomicBool::new(false);
                    check_dfs(
                        || {
                            let sem = Arc::new(BatchSemaphore::new(4, $fairness));
                            sem.acquire_blocking(2).unwrap();
                            let reserver = {
                                let sem = Arc::clone(&sem);
                                thread::spawn(move || {
                                    assert_eq!(me(), 1);
                                    BatchSemaphore::new(0, Fairness::Unfair).release(1);
                                    future::block_on(sem.acquire_reserving(1, 4)).unwrap();
                                    sem.release(4);
                                })
                            };
                            thread::yield_now();
                            assert!(!after(1));
                            match sem.try_upgrade(2, 4) {
                                Ok(()) => sem.release(4),
                                Err(_) => {
                                    REFUSED.store(true, Ordering::SeqCst);
                                    assert!(after(1), "the failed try_upgrade is not after the reservation");
                                    sem.release(2);
                                }
                            }
                            reserver.join().unwrap();
                        },
                        None,
                    );
                    assert!(
                        REFUSED.load(Ordering::SeqCst),
                        "no schedule refused the try_upgrade"
                    );
                }

                /// The holder of the reservation follows the task that polls its `Acquire`, as a
                /// queued waiter does (see `queued_acquire_polled_by_second_task_outlives_its_first_task`):
                /// here the task that reserved finishes once a second task has polled the
                /// `Acquire`, and the release that frees the rest of its permits must wake the
                /// second task, rather than drop the reservation as one of a finished task (see
                /// `reservation_of_finished_task_is_dropped`).
                #[test_log::test]
                fn reservation_polled_by_second_task_outlives_its_first_task() {
                    use std::pin::Pin;
                    type Stash = Arc<Mutex<Option<Pin<Box<Acquire<'static>>>>>>;
                    shuttle::lazy_static! {
                        static ref SEM: BatchSemaphore = BatchSemaphore::new(2, $fairness);
                    }

                    check_dfs(
                        || {
                            SEM.try_acquire(1).unwrap();
                            let stash: Stash = Arc::new(Mutex::new(None));
                            let (reserved_tx, reserved_rx) = shuttle::sync::mpsc::channel::<()>();
                            let (polled_tx, polled_rx) = shuttle::sync::mpsc::channel::<()>();
                            let first = {
                                let stash = Arc::clone(&stash);
                                thread::spawn(move || {
                                    future::block_on(async {
                                        let mut acquire = Box::pin(SEM.acquire_reserving(1, 2));
                                        // One permit is free, so this reserves the semaphore, for
                                        // this task.
                                        assert!(futures::poll!(acquire.as_mut()).is_pending());
                                        *stash.lock().unwrap() = Some(acquire);
                                    });
                                    reserved_tx.send(()).unwrap();
                                    polled_rx.recv().unwrap();
                                })
                            };
                            let second = {
                                let stash = Arc::clone(&stash);
                                thread::spawn(move || {
                                    reserved_rx.recv().unwrap();
                                    let mut acquire = stash.lock().unwrap().take().unwrap();
                                    future::block_on(async {
                                        // Still waits for a permit, now for this task.
                                        assert!(futures::poll!(acquire.as_mut()).is_pending());
                                        polled_tx.send(()).unwrap();
                                        acquire.await.unwrap();
                                    });
                                    SEM.release(2);
                                })
                            };
                            first.join().unwrap(); // the task that reserved has finished
                            SEM.release(1); // frees the last permit that the reservation waits for
                            second.join().unwrap();
                        },
                        None,
                    );
                }
            }
        };
    }

    #[test_log::test]
    #[should_panic(expected = "only an unfair semaphore supports reservations")]
    fn reserving_on_a_fair_semaphore_panics() {
        check_dfs(
            || {
                let sem = BatchSemaphore::new(1, Fairness::StrictlyFair);
                drop(sem.acquire_reserving(1, 1));
            },
            None,
        );
    }

    reservation_tests!(unfair, Fairness::Unfair);

    reservation_tests!(parking_lot, Fairness::ParkingLot);
}

/// Tests of `BatchSemaphore::load_permits`: a read of the semaphore's state with one scheduling
/// point and no effect.
mod load_permits_tests {
    use super::*;

    /// `load_permits` reports the free permits, zero while a reservation lasts, and `None` once the
    /// semaphore is closed.
    #[test_log::test]
    fn load_permits_reads_the_state() {
        check_dfs(
            || {
                future::block_on(async {
                    let sem = BatchSemaphore::new(3, Fairness::Unfair);
                    assert_eq!(sem.load_permits(), Some(3));
                    sem.acquire(1).await.unwrap();
                    assert_eq!(sem.load_permits(), Some(2));

                    let mut reserve = Box::pin(sem.acquire_reserving(2, 3));
                    assert!(futures::poll!(reserve.as_mut()).is_pending());
                    assert_eq!(sem.load_permits(), Some(0), "a reservation keeps the permits");
                    drop(reserve);
                    assert_eq!(sem.load_permits(), Some(2));

                    sem.close();
                    assert_eq!(sem.load_permits(), None);
                });
            },
            None,
        );
    }

    /// `load_permits` changes nothing, but it is a scheduling point: another task can run between
    /// two reads, so two consecutive reads can disagree. Reading with `try_acquire` + `release`
    /// instead would transiently take the permit, which would make the other task's `try_acquire`
    /// fail.
    #[test_log::test]
    fn load_permits_is_a_scheduling_point_without_effects() {
        let observed = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let observed_clone = Arc::clone(&observed);
        check_dfs(
            move || {
                let sem = Arc::new(BatchSemaphore::new(1, Fairness::Unfair));
                let other = {
                    let sem = sem.clone();
                    thread::spawn(move || {
                        // The permit is only ever held here, so only a probe with effects could
                        // make this fail.
                        sem.try_acquire(1).expect("a load_permits took the permit");
                        sem.release(1);
                    })
                };
                let first = sem.load_permits().unwrap();
                let second = sem.load_permits().unwrap();
                observed_clone.lock().unwrap().insert((first, second));
                other.join().unwrap();
            },
            None,
        );
        let observed = Arc::try_unwrap(observed).unwrap().into_inner().unwrap();
        assert_eq!(
            observed,
            HashSet::from([(1, 1), (1, 0), (0, 0), (0, 1)]),
            "each read must have its own scheduling point"
        );
    }

    /// A read is causally after the acquires before it, as a load of an atomic word is after the
    /// stores to it, and as a failed `try_acquire` is. Task 1 releases another semaphore first, so
    /// that its clock has an event of its own when it takes the permit. `released` is a `std`
    /// atomic, which adds no scheduling point and no clock edge.
    #[test_log::test]
    fn load_permits_is_after_the_acquires() {
        for fairness in [Fairness::StrictlyFair, Fairness::Unfair, Fairness::ParkingLot] {
            check_dfs(
                move || {
                    let sem = Arc::new(BatchSemaphore::new(1, fairness));
                    let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let other = {
                        let (sem, released) = (Arc::clone(&sem), Arc::clone(&released));
                        thread::spawn(move || {
                            assert_eq!(me(), 1);
                            BatchSemaphore::new(0, Fairness::Unfair).release(1);
                            sem.acquire_blocking(1).unwrap();
                            sem.release(1);
                            released.store(true, Ordering::SeqCst);
                        })
                    };
                    let taken = sem.load_permits() == Some(0);
                    if taken || released.load(Ordering::SeqCst) {
                        check_clock(|i, c| (i != 1) || (c > 0));
                    } else {
                        check_clock(|i, c| (i != 1) || (c == 0));
                    }
                    other.join().unwrap();
                },
                None,
            );
        }
    }

    /// On a ParkingLot semaphore, a read is also causally after the releases before it and the
    /// close, so that a replay up to its clock (see `ReplayScheduler::set_target_clock`) reads the
    /// same state. Here task 1, which has an event of its own first, releases a permit, or closes
    /// the semaphore, and the main task reads the change.
    #[test_log::test]
    fn parking_lot_load_permits_is_after_the_releases_and_the_close() {
        static SAW_CHANGE: AtomicUsize = AtomicUsize::new(0);
        for close in [false, true] {
            SAW_CHANGE.store(0, Ordering::SeqCst);
            check_dfs(
                move || {
                    let sem = Arc::new(BatchSemaphore::new(0, Fairness::ParkingLot));
                    let other = {
                        let sem = Arc::clone(&sem);
                        thread::spawn(move || {
                            assert_eq!(me(), 1);
                            BatchSemaphore::new(0, Fairness::Unfair).release(1);
                            if close {
                                sem.close();
                            } else {
                                sem.release(1);
                            }
                        })
                    };
                    if sem.load_permits() != Some(0) {
                        SAW_CHANGE.fetch_add(1, Ordering::SeqCst);
                        check_clock(|i, c| (i != 1) || (c > 0));
                    }
                    other.join().unwrap();
                },
                None,
            );
            assert!(SAW_CHANGE.load(Ordering::SeqCst) > 0, "no schedule read the change");
        }
    }

    /// The same for a reservation that its request gives up: the read that sees the permit that it
    /// kept is after the request gave it up. `CANCELLED` records task 1's clock right before the
    /// drop, with no scheduling point between them, so if the main task sees it, the drop came
    /// before the main task's read.
    #[test_log::test]
    fn parking_lot_load_permits_is_after_a_cancelled_reservation() {
        static CANCELLED: AtomicUsize = AtomicUsize::new(0);
        static CHECKED: AtomicUsize = AtomicUsize::new(0);
        check_dfs(
            || {
                CANCELLED.store(0, Ordering::SeqCst);
                let sem = Arc::new(BatchSemaphore::new(2, Fairness::ParkingLot));
                sem.try_acquire(1).unwrap();
                let canceller = {
                    let sem = Arc::clone(&sem);
                    thread::spawn(move || {
                        assert_eq!(me(), 1);
                        future::block_on(async {
                            let mut reserve = Box::pin(sem.acquire_reserving(1, 2));
                            // One permit is free, so this reserves the semaphore.
                            assert!(futures::poll!(reserve.as_mut()).is_pending());
                            BatchSemaphore::new(0, Fairness::Unfair).release(1);
                            let at = clock_entry(1);
                            drop(reserve);
                            CANCELLED.store(at, Ordering::SeqCst);
                        });
                    })
                };
                let seen = sem.load_permits();
                let cancelled = CANCELLED.load(Ordering::SeqCst);
                if cancelled > 0 && seen == Some(1) {
                    CHECKED.fetch_add(1, Ordering::SeqCst);
                    assert!(clock_entry(1) >= cancelled, "the read is not after the cancel");
                }
                canceller.join().unwrap();
            },
            None,
        );
        assert!(
            CHECKED.load(Ordering::SeqCst) > 0,
            "no schedule read the cancelled reservation"
        );
    }

    /// The same for a release while a task panics, which closes the semaphore, to model lock
    /// poisoning: the read that sees the close is after it. Task 1 has an event of its own before
    /// it panics, and `POISONED` records its clock then.
    #[test_log::test]
    fn parking_lot_load_permits_is_after_a_release_that_poisons_the_semaphore() {
        static POISONED: AtomicUsize = AtomicUsize::new(0);
        static CHECKED: AtomicUsize = AtomicUsize::new(0);
        struct Guard<'a>(&'a BatchSemaphore);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.release(1);
            }
        }
        check_dfs(
            || {
                POISONED.store(0, Ordering::SeqCst);
                let sem = Arc::new(BatchSemaphore::new(1, Fairness::ParkingLot));
                let poisoner = {
                    let sem = Arc::clone(&sem);
                    thread::spawn(move || {
                        assert_eq!(me(), 1);
                        sem.try_acquire(1).unwrap();
                        BatchSemaphore::new(0, Fairness::Unfair).release(1);
                        POISONED.store(clock_entry(1), Ordering::SeqCst);
                        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let _guard = Guard(&sem);
                            panic!("expected panic");
                        }));
                        assert!(panicked.is_err());
                    })
                };
                if sem.load_permits().is_none() {
                    CHECKED.fetch_add(1, Ordering::SeqCst);
                    assert!(
                        clock_entry(1) >= POISONED.load(Ordering::SeqCst),
                        "the read is not after the poisoning release"
                    );
                }
                poisoner.join().unwrap();
            },
            None,
        );
        assert!(CHECKED.load(Ordering::SeqCst) > 0, "no schedule read the close");
    }

    /// The same for a reservation of a task that has finished, which a woken `Acquire` that is
    /// dropped drops when it passes its wake on (see `Acquire::drop`): the read that sees the
    /// permit that the reservation kept is after the drop. `DROPPED` records the main task's clock
    /// right before the drop, with no scheduling point between them.
    #[test_log::test]
    fn parking_lot_load_permits_is_after_a_dropped_woken_acquire_that_drops_a_stale_reservation() {
        static DROPPED: AtomicUsize = AtomicUsize::new(0);
        static CHECKED: AtomicUsize = AtomicUsize::new(0);
        shuttle::lazy_static! {
            static ref SEM: BatchSemaphore = BatchSemaphore::new(2, Fairness::ParkingLot);
        }
        check_dfs(
            || {
                future::block_on(async {
                    DROPPED.store(0, Ordering::SeqCst);
                    SEM.acquire(2).await.unwrap();
                    let mut woken = Box::pin(SEM.acquire(1));
                    assert!(futures::poll!(woken.as_mut()).is_pending());
                    SEM.release(1); // wakes `woken`, and one permit is free
                    let stash = Arc::new(Mutex::new(None));
                    let registrant = future::spawn({
                        let stash = Arc::clone(&stash);
                        async move {
                            let mut reserve = Box::pin(SEM.acquire_reserving(1, 2));
                            // One permit is free, so this reserves the semaphore.
                            assert!(futures::poll!(reserve.as_mut()).is_pending());
                            *stash.lock().unwrap() = Some(reserve);
                        }
                    });
                    registrant.await.unwrap(); // the holder's task has finished
                    let reader = thread::spawn(|| {
                        if SEM.load_permits() == Some(1) {
                            let dropped = DROPPED.load(Ordering::SeqCst);
                            assert!(dropped > 0, "the stale reservation kept the permit");
                            CHECKED.fetch_add(1, Ordering::SeqCst);
                            assert!(clock_entry(0) >= dropped, "the read is not after the drop");
                        }
                    });
                    BatchSemaphore::new(0, Fairness::Unfair).release(1);
                    DROPPED.store(clock_entry(0), Ordering::SeqCst);
                    drop(woken); // passes the wake on, which drops the stale reservation
                    reader.join().unwrap();
                    drop(stash.lock().unwrap().take());
                    SEM.release(1);
                });
            },
            None,
        );
        assert!(
            CHECKED.load(Ordering::SeqCst) > 0,
            "no schedule read the dropped reservation"
        );
    }
}

/// Tests of `BatchSemaphore::has_waiters`: whether a request waits in the semaphore's queue.
mod has_waiters_tests {
    use super::*;

    /// A request counts while it is in the queue. A strictly fair release grants the waiter its
    /// permits, which takes it out of the queue, and a ParkingLot one takes it out of the queue to
    /// wake it, while an unfair one wakes it, and it stays in the queue until it tries again.
    #[test_log::test]
    fn has_waiters_is_whether_a_request_is_queued() {
        for fairness in [Fairness::StrictlyFair, Fairness::Unfair, Fairness::ParkingLot] {
            check_dfs(
                move || {
                    future::block_on(async move {
                        let sem = BatchSemaphore::new(1, fairness);
                        assert!(!sem.has_waiters());
                        sem.acquire(1).await.unwrap();
                        let mut waiting = Box::pin(sem.acquire(1));
                        assert!(futures::poll!(waiting.as_mut()).is_pending());
                        assert!(sem.has_waiters());
                        sem.release(1);
                        assert_eq!(sem.has_waiters(), fairness == Fairness::Unfair);
                        waiting.await.unwrap();
                        assert!(!sem.has_waiters());
                        sem.release(1);
                    });
                },
                None,
            );
        }
    }

    /// The holder of the reservation is not in the queue, and a request that waits behind it is.
    #[test_log::test]
    fn the_reservation_holder_does_not_count() {
        check_dfs(
            || {
                future::block_on(async {
                    let sem = BatchSemaphore::new(2, Fairness::Unfair);
                    sem.acquire(1).await.unwrap();
                    let mut reserve = Box::pin(sem.acquire_reserving(1, 2));
                    assert!(futures::poll!(reserve.as_mut()).is_pending());
                    assert!(!sem.has_waiters());
                    let mut waiting = Box::pin(sem.acquire(1));
                    assert!(futures::poll!(waiting.as_mut()).is_pending());
                    assert!(sem.has_waiters());
                    drop(waiting);
                    drop(reserve);
                    sem.release(1);
                });
            },
            None,
        );
    }

    /// A request whose task has finished without dropping its `Acquire` waits for nobody, so it
    /// does not count (see `release_to_waiter_of_finished_task`).
    #[test_log::test]
    fn a_request_of_a_finished_task_does_not_count() {
        shuttle::lazy_static! {
            static ref SEM: BatchSemaphore = BatchSemaphore::new(0, Fairness::StrictlyFair);
        }

        check_dfs(
            || {
                future::block_on(async {
                    let acquire = Arc::new(Mutex::new(None));
                    let registrant = future::spawn({
                        let acquire = Arc::clone(&acquire);
                        async move {
                            let mut acq = Box::pin(SEM.acquire(1));
                            assert!(futures::poll!(acq.as_mut()).is_pending());
                            assert!(SEM.has_waiters());
                            *acquire.lock().unwrap() = Some(acq);
                        }
                    });
                    registrant.await.unwrap();
                    assert!(!SEM.has_waiters());
                    drop(acquire.lock().unwrap().take());
                });
            },
            None,
        );
    }

    /// On a ParkingLot semaphore, a `has_waiters` that sees a request is causally after the
    /// request joined the queue, as a load of `PARKED_BIT` is after the thread that set it, so a
    /// replay up to its clock sees the request too. Task 1 releases another semaphore first, so
    /// that its clock has an event of its own when it joins the queue. The main task reads the
    /// state as a lock's `bump` does, with `load_permits` first, whose scheduling point lets task
    /// 1 join the queue.
    #[test_log::test]
    fn parking_lot_has_waiters_is_after_the_requests_that_wait() {
        static SAW_WAITER: AtomicUsize = AtomicUsize::new(0);
        check_dfs(
            || {
                let sem = Arc::new(BatchSemaphore::new(0, Fairness::ParkingLot));
                let waiter = {
                    let sem = Arc::clone(&sem);
                    thread::spawn(move || {
                        assert_eq!(me(), 1);
                        BatchSemaphore::new(0, Fairness::Unfair).release(1);
                        sem.acquire_blocking(1).unwrap();
                    })
                };
                assert_eq!(sem.load_permits(), Some(0));
                if sem.has_waiters() {
                    SAW_WAITER.fetch_add(1, Ordering::SeqCst);
                    check_clock(|i, c| (i != 1) || (c > 0));
                } else {
                    check_clock(|i, c| (i != 1) || (c == 0));
                }
                sem.release(1);
                waiter.join().unwrap();
            },
            None,
        );
        assert!(
            SAW_WAITER.load(Ordering::SeqCst) > 0,
            "no schedule had a waiter at the read"
        );
    }
}

// This test exercises scenarios to ensure that the BatchSemaphore behaves correctly in the presence
// of tasks that drop an `Acquire` guard without waiting for the semaphore to become available.
//
// The general idea is that there are 3 types of tasks: `EarlyDrop`, `Hold` and `Release` tasks
// (determined by the `Behavior` enum).
// 1. The semaphore initially has 0 permits.
// 2. The main task spawns a set of tasks, specifying the behavior and the number of permits each task should request
// 3. Each task polls the semaphore once when it is created, in order to get added as a Waiter.
// 4. The main task releases N semaphores, which are sufficient to ensure that all tasks complete (see below)
// 5. Each task then proceeds according to its defined `behavior`:
//    `EarlyDrop` tasks drop their Acquire guards (and are removed from the waiters queue)
//    `Hold` tasks wait to acquire their permits, and then terminate (without releasing any permits)
//    `Release` tasks wait to acquire their permits, and then release their permits and terminate
//
// The value of N is computed as
//     (sum of permits requested by the Hold tasks) + (max over the permits requested by the Release and EarlyDrop tasks)
mod early_acquire_drop_tests {
    use super::*;
    use futures::{
        future::join_all,
        task::{Context, Poll, Waker},
        Future,
    };
    use pin_project::pin_project;
    use proptest::prelude::*;
    use proptest_derive::Arbitrary;
    use shuttle::{
        check_random,
        sync::mpsc::{channel, Sender},
    };
    use std::pin::Pin;

    #[derive(Arbitrary, Clone, Copy, Debug)]
    enum Behavior {
        EarlyDrop, // Task drops before future completes
        Release,   // Task releases permits it acquires
        Hold,      // Task holds permits it acquires
    }

    #[pin_project]
    struct Task {
        poll_count: usize,        // how many times the Future has been polled
        behavior: Behavior,       // how this task should behave
        requested_permits: usize, // how many permits this Task requests
        tx: Sender<Waker>,        // channel for informing the main task that this task is added as a Waiter
        #[pin]
        acquire: Acquire<'static>,
    }

    impl Task {
        fn new(behavior: Behavior, requested_permits: usize, tx: Sender<Waker>, sem: &'static BatchSemaphore) -> Self {
            Self {
                poll_count: 0,
                behavior,
                requested_permits,
                tx,
                acquire: sem.acquire(requested_permits),
            }
        }
    }

    impl Future for Task {
        type Output = usize;

        fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
            let mut this = self.project();
            if *this.poll_count == 0 {
                // The first time we are polled, also poll the inner Acquire handle
                // so this task gets added as a Waiter
                let s: Poll<Result<(), AcquireError>> = this.acquire.as_mut().poll(cx);
                assert!(s.is_pending());
                this.tx.send(cx.waker().clone()).unwrap(); // Notify main task
                *this.poll_count += 1;
                Poll::Pending
            } else if matches!(*this.behavior, Behavior::EarlyDrop) {
                // Since this is an early drop, we got 0 permits
                Poll::Ready(0)
            } else {
                // If not early dropping, wait until the inner Acquire handle successfully gets
                // a permit.  When successful, return the number of permits acquired.
                this.acquire.as_mut().poll(cx).map(|_| *this.requested_permits)
            }
        }
    }

    fn dropped_acquire_must_release(sem: &'static BatchSemaphore, task_config: Vec<(Behavior, usize)>) {
        future::block_on(async move {
            let mut wakers = vec![];
            let mut handles = vec![];

            let mut total_held = 0usize;
            let mut max_requested = 0usize;

            for (behavior, requested_permits) in task_config {
                let (tx, rx) = channel();
                match behavior {
                    Behavior::Hold => total_held += requested_permits,
                    _ => max_requested = std::cmp::max(max_requested, requested_permits),
                }
                handles.push(future::spawn(async move {
                    let task: Task = Task::new(behavior, requested_permits, tx, sem);
                    let p = task.await;
                    // Note: tasks doing an early drop will return p=0, and release(0) is a no-op
                    if matches!(behavior, Behavior::Release) {
                        sem.release(p);
                    }
                }));
                wakers.push(rx.recv().unwrap());
            }

            sem.release(total_held + max_requested);
            for w in wakers.into_iter() {
                w.wake();
            }

            join_all(handles).await;
        });
    }

    macro_rules! sem_tests {
        ($mod_name:ident, $fairness:expr) => {
            mod $mod_name {
                use super::*;

                #[test_log::test]
                fn dropped_acquire_must_release_exhaustive() {
                    shuttle::lazy_static! {
                        static ref SEM: BatchSemaphore = BatchSemaphore::new(0, $fairness);
                    }
                    check_dfs(
                        || dropped_acquire_must_release(&SEM, vec![(Behavior::EarlyDrop, 1), (Behavior::Release, 1)]),
                        None,
                    );
                }

                #[test_log::test]
                fn dropped_acquire_must_release_deadlock() {
                    shuttle::lazy_static! {
                        static ref SEM: BatchSemaphore = BatchSemaphore::new(0, $fairness);
                    }
                    check_dfs(
                        || dropped_acquire_must_release(&SEM, vec![(Behavior::Hold, 1), (Behavior::EarlyDrop, 2), (Behavior::Release, 1)]),
                        None,
                    );
                }

                const MAX_REQUESTED_PERMITS: usize = 3;
                const MAX_TASKS: usize = 7;

                proptest! {
                    #[test_log::test]
                    fn dropped_acquire_must_release_random(behavior in proptest::collection::vec((proptest::arbitrary::any::<Behavior>(), 1..=MAX_REQUESTED_PERMITS), 1..=MAX_TASKS)) {
                        check_random(
                            move || {
                                shuttle::lazy_static! {
                                    static ref SEM: BatchSemaphore = BatchSemaphore::new(0, $fairness);
                                }
                                dropped_acquire_must_release(&SEM, behavior.clone())
                            },
                            1000,
                        );
                    }
                }
            }
        }
    }

    sem_tests!(unfair, Fairness::Unfair);

    sem_tests!(fair, Fairness::StrictlyFair);

    sem_tests!(parking_lot, Fairness::ParkingLot);
}

/// Tests of a `Fairness::ParkingLot` semaphore. Most drive several acquires from one task with
/// `futures::poll!`, so that each step is deterministic. A parked waiter does not try again when it
/// is polled, so polling one shows whether a release woke it, and a woken one tries when it is
/// polled, like a new request.
mod parking_lot_tests {
    use super::*;
    use futures::future::{select, Either};
    use shuttle::scheduler::{
        DfsScheduler, PctScheduler, RandomScheduler, ReplayScheduler, Schedule, Scheduler, Task, TaskId,
    };
    use shuttle::{Config, FailurePersistence, MaxSteps, Runner};
    use std::sync::atomic::AtomicBool;
    use std::task::Poll;

    fn sem(num_permits: usize) -> BatchSemaphore {
        BatchSemaphore::new(num_permits, Fairness::ParkingLot)
    }

    // The permits of an `RwLock`, as in `shuttle-parking_lot`, with fewer of them: a reader (`S`)
    // takes one, an upgradable reader (`U`) a majority, and a writer (`X`) all of them. A writer
    // reserves the lock as soon as no other writer or upgradable reader holds it, as a
    // `parking_lot` writer sets `WRITER_BIT` (see `BatchSemaphore::acquire_reserving`).
    const N: usize = 8;
    const UPG: usize = N / 2 + 1;
    const RESERVE_AT: usize = N - UPG + 1;

    /// Poll a pinned `Acquire` once, in an async block, and check that it is pending.
    macro_rules! pending {
        ($acquire:ident) => {
            assert!(
                futures::poll!($acquire.as_mut()).is_pending(),
                concat!("`", stringify!($acquire), "` is not pending")
            )
        };
    }

    /// Poll a pinned `Acquire` once, in an async block, and check that it has its permits.
    macro_rules! ready {
        ($acquire:ident) => {
            assert!(
                matches!(futures::poll!($acquire.as_mut()), Poll::Ready(Ok(()))),
                concat!("`", stringify!($acquire), "` does not have its permits")
            )
        };
    }

    /// Run `f` under `scheduler` with `parking_lot_fairness_bound` as the bound, and return the
    /// number of executions.
    fn run_with_bound<S, F>(scheduler: S, bound: usize, max_steps: usize, f: F) -> usize
    where
        S: Scheduler + 'static,
        F: Fn() + Send + Sync + 'static,
    {
        let mut config = Config::new();
        config.parking_lot_fairness_bound = bound;
        config.max_steps = MaxSteps::FailAfter(max_steps);
        Runner::new(scheduler, config).run(f)
    }

    /// Run `f` under exhaustive DFS with `parking_lot_fairness_bound` as the bound.
    fn check_dfs_with_bound<F>(bound: usize, f: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        run_with_bound(DfsScheduler::new(None, false), bound, 1_000, f);
    }

    /// A plain release wakes the waiter at the front of the queue only, as the unlock of a
    /// `parking_lot` `Mutex` does, and leaves the permit free. So another request can take it
    /// first: `try_acquire` (a `Mutex::try_lock`) ignores the waiter that is still parked, while
    /// `try_acquire_without_overtaking` (an `RwLock::try_write`) fails behind it. The woken waiter
    /// that loses parks again, at the back, so the next release wakes the one that was behind it.
    #[test_log::test]
    fn plain_release_wakes_the_head_and_a_loser_parks_at_the_back() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b);
                    assert!(s.has_waiters());

                    s.release(1); // wakes a, but not b
                    assert_eq!(s.available_permits(), 1, "a plain release handed the permit off");
                    assert!(s.has_waiters(), "b is still parked");
                    assert_eq!(s.try_acquire_without_overtaking(1), Err(TryAcquireError::NoPermits));
                    s.try_acquire(1).unwrap();
                    pending!(a); // lost, so it parks again, behind b

                    s.release(1); // wakes b
                    pending!(a); // parked, so it does not even try
                    ready!(b);
                    s.release(1); // wakes a
                    ready!(a);
                    assert!(!s.has_waiters());
                    s.release(1);
                });
            },
            None,
        );
    }

    /// A fair release grants the waiter at the front its permit inside the release, so that no
    /// other request can take it first, and leaves the next waiter parked.
    #[test_log::test]
    fn fair_release_hands_the_permits_to_the_waiters_it_lets_in() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b);

                    s.release_fair(1);
                    assert_eq!(s.available_permits(), 0);
                    assert_eq!(s.try_acquire(1), Err(TryAcquireError::NoPermits));
                    pending!(b);
                    ready!(a);
                    s.release_fair(1);
                    ready!(b);
                    s.release_fair(1);
                    assert_eq!(s.available_permits(), 1, "a fair release that lets nobody in is plain");
                });
            },
            None,
        );
    }

    /// The wake filter of an `RwLock` unlock: the readers up to the first writer, which then
    /// reserves the lock behind them, and not the reader behind it (`[S, S, X, S]`).
    #[test_log::test]
    fn release_wakes_the_readers_up_to_the_first_writer() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(N);
                    s.acquire(N).await.unwrap();
                    let mut r1 = Box::pin(s.acquire(1));
                    pending!(r1);
                    let mut r2 = Box::pin(s.acquire(1));
                    pending!(r2);
                    let mut w = Box::pin(s.acquire_reserving(RESERVE_AT, N));
                    pending!(w);
                    let mut r3 = Box::pin(s.acquire(1));
                    pending!(r3);

                    s.release(N);
                    pending!(r3);
                    ready!(r1);
                    ready!(r2);
                    pending!(w); // reserves the lock: the readers are in
                    assert_eq!(s.available_permits(), 0);
                    s.release_without_waking(1);
                    s.release_without_waking(1); // the last reader wakes the writer
                    ready!(w);
                    s.release(N); // wakes r3
                    ready!(r3);
                    s.release_without_waking(1);
                });
            },
            None,
        );
    }

    /// Behind an upgradable reader that a release lets in, the release passes over the writers and
    /// the upgradable readers, but lets every reader in: `[U, X, S, U, S]` wakes `U, S, S`.
    #[test_log::test]
    fn upgradable_reader_in_the_set_passes_over_writers_but_not_readers() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(N);
                    s.acquire(N).await.unwrap();
                    let mut u1 = Box::pin(s.acquire(UPG));
                    pending!(u1);
                    let mut w = Box::pin(s.acquire_reserving(RESERVE_AT, N));
                    pending!(w);
                    let mut r1 = Box::pin(s.acquire(1));
                    pending!(r1);
                    let mut u2 = Box::pin(s.acquire(UPG));
                    pending!(u2);
                    let mut r2 = Box::pin(s.acquire(1));
                    pending!(r2);

                    s.release(N);
                    pending!(w);
                    pending!(u2);
                    ready!(r2);
                    ready!(r1);
                    ready!(u1);
                    assert_eq!(s.available_permits(), N - UPG - 2);
                    s.release_without_waking(1);
                    s.release_without_waking(1);
                    s.release(UPG); // wakes w, whose permits are all free
                    pending!(u2);
                    ready!(w);
                    s.release(N); // wakes u2
                    ready!(u2);
                    s.release(UPG);
                });
            },
            None,
        );
    }

    /// `downgrade_to_upgradable` keeps an upgradable read, so the release passes over the writers
    /// and the upgradable readers, and wakes only the readers.
    #[test_log::test]
    fn release_that_keeps_an_upgradable_read_wakes_only_readers() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(N);
                    s.acquire(N).await.unwrap();
                    let mut w = Box::pin(s.acquire_reserving(RESERVE_AT, N));
                    pending!(w);
                    let mut r = Box::pin(s.acquire(1));
                    pending!(r);
                    let mut u = Box::pin(s.acquire(UPG));
                    pending!(u);

                    s.release(N - UPG);
                    pending!(w);
                    pending!(u);
                    assert_eq!(s.available_permits(), N - UPG);
                    ready!(r);
                    drop((w, u));
                });
            },
            None,
        );
    }

    /// A fair release grants the readers that it lets in, and hands the writer behind them the
    /// reservation, inside the release, as a fair `parking_lot` unlock hands it `WRITER_BIT`.
    #[test_log::test]
    fn fair_release_hands_the_writer_behind_the_readers_the_reservation() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(N);
                    s.acquire(N).await.unwrap();
                    let mut r1 = Box::pin(s.acquire(1));
                    pending!(r1);
                    let mut w = Box::pin(s.acquire_reserving(RESERVE_AT, N));
                    pending!(w);
                    let mut r2 = Box::pin(s.acquire(1));
                    pending!(r2);

                    s.release_fair(N);
                    assert_eq!(s.available_permits(), 0, "the writer does not hold the reservation");
                    assert_eq!(s.try_acquire(1), Err(TryAcquireError::NoPermits));
                    ready!(r1);
                    s.release_without_waking(1); // the last reader wakes the writer
                    ready!(w);
                    pending!(r2);
                    s.release(N);
                    ready!(r2);
                    s.release_without_waking(1);
                });
            },
            None,
        );
    }

    /// A writer that reserved the lock waits for the readers to leave, and the last one's
    /// `release_without_waking` (`unlock_shared`) wakes it, as the last reader to leave wakes a
    /// `parking_lot` writer that waits for the readers.
    #[test_log::test]
    fn shared_release_wakes_a_writer_that_waits_for_the_readers() {
        check_dfs(
            || {
                let s = Arc::new(sem(N));
                s.try_acquire(1).unwrap(); // a reader
                let writer = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || {
                        future::block_on(s.acquire_reserving(RESERVE_AT, N)).unwrap();
                        s.release(N);
                    })
                };
                s.release_without_waking(1); // the last reader leaves
                writer.join().unwrap();
            },
            None,
        );
    }

    /// `release_without_waking` (`parking_lot`'s `unlock_shared`) wakes nobody in the queue. A
    /// reader that parked behind a writer stays parked while an upgradable reader that barged in
    /// holds the lock, although its permit is free, until the upgradable reader leaves (the
    /// stranded reader of `parking_lot`).
    #[test_log::test]
    fn shared_release_leaves_a_stranded_reader_parked() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(N);
                    s.acquire(N).await.unwrap();
                    let mut w = Box::pin(s.acquire_reserving(RESERVE_AT, N));
                    pending!(w);
                    let mut r = Box::pin(s.acquire(1));
                    pending!(r);

                    s.release(N); // wakes w only
                    s.acquire(UPG).await.unwrap(); // an upgradable reader barges in
                    pending!(w); // lost, so it parks again, behind r
                    s.acquire(1).await.unwrap(); // another reader barges in
                    s.release_without_waking(1);
                    pending!(r);
                    assert_eq!(s.available_permits(), N - UPG);
                    s.release(UPG); // wakes r and w
                    ready!(r);
                    pending!(w); // reserves the lock
                    s.release_without_waking(1);
                    ready!(w);
                    s.release(N);
                });
            },
            None,
        );
    }

    /// While a reservation holds the semaphore, a plain release only wakes the holder once all of
    /// its permits are free, and leaves the queue alone, as the threads parked on a `parking_lot`
    /// lock stay parked while `WRITER_BIT` is set. Here `a` and `b` are upgradable readers parked
    /// behind a writer that reserved the lock, so a release that wakes one of them passes over the
    /// other.
    #[test_log::test]
    fn plain_release_while_reserved_leaves_the_queue_alone() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(N);
                    s.acquire(2).await.unwrap(); // two readers
                    let mut w = Box::pin(s.acquire_reserving(RESERVE_AT, N));
                    pending!(w); // reserves
                    let mut a = Box::pin(s.acquire(UPG));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(UPG));
                    pending!(b);
                    s.release(1); // one reader leaves, with a plain release
                    pending!(a); // still parked, so it does not try
                    s.release_without_waking(1); // the last reader wakes the writer
                    ready!(w);
                    s.release(N); // wakes a, at the front, and passes over b
                    ready!(a);
                    pending!(b);
                    s.release(UPG);
                    ready!(b);
                    s.release(UPG);
                });
            },
            None,
        );
    }

    /// `try_acquire_without_overtaking` fails while a request is parked, even on free permits, as
    /// `parking_lot`'s `try_write` fails while `PARKED_BIT` is set, and `try_acquire` takes them,
    /// as its `try_read` does. A woken request is not parked.
    #[test_log::test]
    fn try_acquire_without_overtaking_fails_behind_a_parked_waiter_only() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(N);
                    s.acquire(N).await.unwrap();
                    let mut w1 = Box::pin(s.acquire_reserving(RESERVE_AT, N));
                    pending!(w1);
                    let mut w2 = Box::pin(s.acquire_reserving(RESERVE_AT, N));
                    pending!(w2);

                    s.release(N); // wakes w1 only
                    assert_eq!(s.available_permits(), N);
                    assert_eq!(s.try_acquire_without_overtaking(N), Err(TryAcquireError::NoPermits));
                    s.try_acquire(N).unwrap();
                    s.release(N); // wakes w2 too
                    assert!(!s.has_waiters());
                    s.try_acquire_without_overtaking(N).unwrap();
                    pending!(w1);
                    pending!(w2);
                    s.release(N);
                    ready!(w1);
                    s.release(N);
                    ready!(w2);
                    s.release(N);
                });
            },
            None,
        );
    }

    /// `try_acquire_without_overtaking` has a scheduling point of its own, as `parking_lot`'s
    /// `try_write` is a visible operation: right after the main task spawns the other task, which
    /// can hold the permit, some schedule has it fail.
    #[test_log::test]
    fn try_acquire_without_overtaking_is_a_scheduling_point() {
        static FAILED: AtomicBool = AtomicBool::new(false);
        check_dfs(
            || {
                let s = Arc::new(sem(1));
                let other = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || {
                        if s.try_acquire(1).is_ok() {
                            s.release(1);
                        }
                    })
                };
                match s.try_acquire_without_overtaking(1) {
                    Ok(()) => s.release(1),
                    Err(_) => FAILED.store(true, Ordering::SeqCst),
                }
                other.join().unwrap();
            },
            None,
        );
        assert!(
            FAILED.load(Ordering::SeqCst),
            "no schedule ran the other task before the try"
        );
    }

    /// A parked request of a task that has finished without dropping its `Acquire` waits for
    /// nobody, so `try_acquire_without_overtaking` ignores it, as `has_waiters` does.
    #[test_log::test]
    fn try_acquire_without_overtaking_ignores_a_waiter_of_a_finished_task() {
        shuttle::lazy_static! {
            static ref SEM: BatchSemaphore = BatchSemaphore::new(2, Fairness::ParkingLot);
        }
        check_dfs(
            || {
                future::block_on(async {
                    SEM.acquire(1).await.unwrap();
                    let acquire = Arc::new(Mutex::new(None));
                    let registrant = future::spawn({
                        let acquire = Arc::clone(&acquire);
                        async move {
                            let mut acq = Box::pin(SEM.acquire(2));
                            pending!(acq);
                            *acquire.lock().unwrap() = Some(acq);
                        }
                    });
                    registrant.await.unwrap();
                    assert!(!SEM.has_waiters());
                    SEM.try_acquire_without_overtaking(1).unwrap();
                    SEM.release(2);
                    drop(acquire.lock().unwrap().take());
                });
            },
            None,
        );
    }

    /// `try_acquire_without_overtaking` fails while a request waits in the queue on the other
    /// modes too, also where a `try_acquire` would take the free permits ahead of it, as on an
    /// unfair semaphore.
    #[test_log::test]
    fn try_acquire_without_overtaking_fails_behind_a_queued_waiter_on_every_mode() {
        for fairness in [Fairness::StrictlyFair, Fairness::Unfair, Fairness::ParkingLot] {
            check_dfs(
                move || {
                    future::block_on(async move {
                        let s = BatchSemaphore::new(2, fairness);
                        s.acquire(1).await.unwrap();
                        let mut w = Box::pin(s.acquire(2));
                        pending!(w); // waits in the queue for both permits
                        assert_eq!(
                            s.try_acquire_without_overtaking(1),
                            Err(TryAcquireError::NoPermits),
                            "{fairness:?}"
                        );
                        drop(w);
                        s.try_acquire_without_overtaking(1).unwrap();
                        s.release(2);
                    });
                },
                None,
            );
        }
    }

    /// On every mode, `try_acquire_without_overtaking` of a closed semaphore fails with `Closed`,
    /// also after a waiter was queued, before it looks at the permits, so also for none.
    #[test_log::test]
    fn try_acquire_without_overtaking_fails_with_closed_on_a_closed_semaphore() {
        for fairness in [Fairness::StrictlyFair, Fairness::Unfair, Fairness::ParkingLot] {
            check_dfs(
                move || {
                    future::block_on(async move {
                        let s = BatchSemaphore::new(1, fairness);
                        s.acquire(1).await.unwrap();
                        let mut w = Box::pin(s.acquire(1));
                        pending!(w);
                        s.close();
                        for num_permits in [0, 1] {
                            assert_eq!(
                                s.try_acquire_without_overtaking(num_permits),
                                Err(TryAcquireError::Closed),
                                "{fairness:?}"
                            );
                        }
                        assert!(w.await.is_err());
                    });
                },
                None,
            );
        }
    }

    /// A downgrade is a plain release of some of the permits that a task holds: it wakes the
    /// waiters that the free permits let in, and hands nothing off, unless they have been
    /// overtaken `parking_lot_fairness_bound` times. At bound 0, every release grants the waiters
    /// that it wakes.
    #[test_log::test]
    fn downgrade_wakes_without_hand_off_below_the_bound() {
        for bound in [0, 2] {
            check_dfs_with_bound(bound, move || {
                future::block_on(async move {
                    let s = sem(N);
                    s.acquire(N).await.unwrap();
                    let mut r = Box::pin(s.acquire(1));
                    pending!(r);

                    s.release(N - 1);
                    let granted = if bound == 0 { 1 } else { 0 };
                    assert_eq!(s.available_permits(), N - 1 - granted);
                    ready!(r);
                    s.release_without_waking(1);
                    s.release_without_waking(1);
                });
            });
        }
    }

    /// The eventual-fairness stand-in: once other requests have overtaken a waiter `bound` times,
    /// here by taking the permit that each release woke it for, the next release that leaves its
    /// permits free grants them to it.
    #[test_log::test]
    fn waiter_that_lost_bound_races_is_granted_at_the_next_release() {
        for bound in 0..4 {
            check_dfs_with_bound(bound, move || {
                future::block_on(async move {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    for _ in 0..bound {
                        s.release(1);
                        assert_eq!(s.available_permits(), 1, "a release handed off too early");
                        s.try_acquire(1).unwrap();
                        pending!(a); // lost
                    }
                    s.release(1);
                    assert_eq!(s.available_permits(), 0, "the release did not grant the waiter");
                    ready!(a);
                    s.release(1);
                });
            });
        }
    }

    /// Each release that finds a woken waiter that has not tried yet, because its task does not
    /// run, overtakes it too, but not the release that woke it.
    #[test_log::test]
    fn woken_waiter_passed_bound_times_is_granted() {
        for bound in 0..4 {
            check_dfs_with_bound(bound, move || {
                future::block_on(async move {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    s.release(1); // wakes a
                    for _ in 0..bound {
                        assert_eq!(s.available_permits(), 1, "a release handed off too early");
                        s.try_acquire(1).unwrap();
                        s.release(1);
                    }
                    assert_eq!(s.available_permits(), 0, "the release did not grant the waiter");
                    ready!(a);
                    s.release(1);
                });
            });
        }
    }

    /// A shared release (`release_without_waking`) overtakes a woken waiter too, and so does a
    /// release while a reservation holds the semaphore, as a read-spinner releases no other way.
    /// Here `r` is woken, and the release by a reader that barged in passes it while a writer
    /// waits for that reader, and so does the writer's release, which grants `r` at the default
    /// bound of 2.
    #[test_log::test]
    fn shared_release_while_reserved_overtakes_the_woken_waiters() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(N);
                    s.acquire(N).await.unwrap();
                    let mut r = Box::pin(s.acquire(1));
                    pending!(r);
                    s.release(N); // wakes r
                    s.acquire(1).await.unwrap(); // a reader barges in
                    let mut w = Box::pin(s.acquire_reserving(RESERVE_AT, N));
                    pending!(w); // reserves, and waits for the reader
                    s.release_without_waking(1); // passes r once, and wakes w
                    ready!(w);
                    s.release(N); // passes r again: the stand-in grants it
                    assert_eq!(s.available_permits(), N - 1, "the releases did not pass r");
                    ready!(r);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// A release overtakes a woken waiter also when it leaves too few permits free for it, as the
    /// woken `parking_lot` thread would have lost if it had tried then. Here two readers' releases
    /// pass a woken writer while an upgradable reader holds the lock, so it starves, and parks
    /// again at the second, and the upgradable reader's release lets it in, and grants it all of
    /// the permits, at the default bound of 2.
    #[test_log::test]
    fn release_that_leaves_too_few_permits_overtakes_a_woken_waiter() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(N);
                    s.acquire(N).await.unwrap();
                    let mut w = Box::pin(s.acquire_reserving(RESERVE_AT, N));
                    pending!(w);
                    s.release(N); // wakes w
                    s.acquire(UPG).await.unwrap(); // an upgradable reader barges in
                    s.acquire(1).await.unwrap(); // and two readers
                    s.acquire(1).await.unwrap();
                    s.release_without_waking(1); // passes w once, with too few permits free for it
                    s.release_without_waking(1); // twice: w tries, and loses
                    assert!(s.has_waiters(), "the readers' releases did not overtake w");
                    s.release(UPG); // wakes w, and the stand-in grants it all of the permits
                    assert_eq!(s.available_permits(), 0, "the stand-in did not grant w");
                    ready!(w);
                    s.release(N);
                });
            },
            None,
        );
    }

    /// A read of the state (`load_permits`) overtakes a woken waiter that has not tried yet, as a
    /// release does, and one that has been overtaken `bound` times tries at the read, as a woken
    /// `parking_lot` thread would have by then. Here the permit is free, so it is granted.
    #[test_log::test]
    fn read_grants_a_woken_waiter_that_was_overtaken_bound_times() {
        for bound in 0..4 {
            check_dfs_with_bound(bound, move || {
                future::block_on(async move {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    s.release(1); // wakes a, or at bound 0, grants it
                    for _ in 1..bound {
                        assert_eq!(s.load_permits(), Some(1), "a read granted the waiter too early");
                    }
                    if bound > 0 {
                        assert_eq!(s.load_permits(), Some(0), "the read did not grant the waiter");
                    }
                    ready!(a);
                    s.release(1);
                });
            });
        }
    }

    /// A woken waiter that has been overtaken twice, at the default bound, and whose permit is
    /// held when a read lets it try, lost the race: it parks again, at the back, so `has_waiters`,
    /// the `PARKED_BIT` that `bump` reads, sees it, and the fair release of a `bump` hands it the
    /// permit.
    #[test_log::test]
    fn read_parks_a_starving_woken_waiter_that_cannot_take_its_permits() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    s.release(1); // wakes a
                    s.try_acquire(1).unwrap(); // takes the permit before a tries
                    assert_eq!(s.load_permits(), Some(0)); // passes a once
                    assert!(!s.has_waiters(), "a is woken, not parked");
                    assert_eq!(s.load_permits(), Some(0)); // passes a twice: it tries, and parks
                    assert!(s.has_waiters(), "a did not park again");
                    pending!(a); // parked, so it does not try
                    s.release_fair(1);
                    assert_eq!(s.available_permits(), 0, "the fair release did not hand a the permit");
                    ready!(a);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// A starving woken waiter that loses at a read parks again at the back, as one that loses
    /// its own try does, so the next release wakes the waiter that was behind it.
    #[test_log::test]
    fn read_parks_a_starving_waiter_again_at_the_back() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b);
                    s.release(1); // wakes a
                    s.try_acquire(1).unwrap();
                    assert_eq!(s.load_permits(), Some(0)); // passes a once
                    assert_eq!(s.load_permits(), Some(0)); // twice: a tries, loses, and parks behind b
                    s.release(1); // wakes b, at the front
                    assert_eq!(
                        s.available_permits(),
                        1,
                        "the release granted a, which parked at the front"
                    );
                    pending!(a);
                    ready!(b);
                    s.release(1);
                    ready!(a);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// A starving waiter that a read parks again keeps the ticket of its first park, so it is
    /// still the older request when the stand-in grants: here `a` parks again behind `b` at a
    /// read, both starve at the last release, and `a` is granted.
    #[test_log::test]
    fn waiter_parked_again_at_a_read_keeps_its_seniority() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b);
                    s.release(1); // wakes a
                    s.try_acquire(1).unwrap();
                    assert_eq!(s.load_permits(), Some(0)); // passes a once
                    assert_eq!(s.load_permits(), Some(0)); // twice: a tries, loses, and parks behind b
                    s.release(1); // wakes b
                    s.try_acquire(1).unwrap();
                    assert_eq!(s.load_permits(), Some(0)); // passes b once
                    s.release(1); // passes b twice, and wakes a, which starves too: a is older
                    ready!(a);
                    pending!(b);
                    s.release(1);
                    ready!(b);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// At a read, every starving woken waiter tries, the oldest request first: here both `a` and
    /// `b` starve, one permit is free, `a` is granted it, and `b` parks again.
    #[test_log::test]
    fn read_lets_every_starving_waiter_try_oldest_first() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(2);
                    s.acquire(2).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b);
                    s.release(2); // wakes a and b
                    s.try_acquire(2).unwrap();
                    s.release(1); // passes both once, and frees one permit
                    assert_eq!(s.load_permits(), Some(0)); // twice: a takes the permit, b parks again
                    assert!(s.has_waiters(), "b did not try at the read");
                    ready!(a);
                    pending!(b);
                    s.release(1); // wakes b, which starves: granted
                    ready!(b);
                    s.release(1);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// A starving woken waiter that tries at a read while another request holds the reservation
    /// loses, as a `parking_lot` reader loses while `WRITER_BIT` is set, and parks again: a read
    /// does not let a reader in past a writer that waits for the readers to leave. It still
    /// overtakes the woken waiters while the reservation is held, so a task that waits for them by
    /// reading the state cannot starve them.
    #[test_log::test]
    fn read_does_not_grant_past_a_reservation() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(N);
                    s.acquire(N).await.unwrap();
                    let mut r = Box::pin(s.acquire(1));
                    pending!(r);
                    s.release(N); // wakes r
                    s.acquire(1).await.unwrap(); // a reader barges in
                    let mut w = Box::pin(s.acquire_reserving(RESERVE_AT, N));
                    pending!(w); // reserves, and waits for the reader
                    assert_eq!(s.load_permits(), Some(0)); // passes r once
                    assert_eq!(s.load_permits(), Some(0)); // twice: r tries, and the reservation refuses it
                    assert!(s.has_waiters(), "r did not park again");
                    pending!(r);
                    s.release_without_waking(1); // the reader leaves: wakes w
                    ready!(w);
                    s.release(N); // wakes r, which starves: granted
                    ready!(r);
                    s.release_without_waking(1);
                });
            },
            None,
        );
    }

    /// A read forgets a woken waiter whose task has finished without dropping its `Acquire`, as a
    /// release does (see `woken_waiter_of_a_finished_task_is_forgotten`), rather than grant a
    /// finished task the permit.
    #[test_log::test]
    fn read_forgets_a_woken_waiter_of_a_finished_task() {
        shuttle::lazy_static! {
            static ref SEM: BatchSemaphore = BatchSemaphore::new(1, Fairness::ParkingLot);
        }
        check_dfs(
            || {
                future::block_on(async {
                    SEM.acquire(1).await.unwrap();
                    let acquire = Arc::new(Mutex::new(None));
                    let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
                    let (released_tx, released_rx) = shuttle::sync::mpsc::channel::<()>();
                    let registrant = future::spawn({
                        let acquire = Arc::clone(&acquire);
                        async move {
                            let mut acq = Box::pin(SEM.acquire(1));
                            pending!(acq);
                            *acquire.lock().unwrap() = Some(acq);
                            parked_tx.send(()).unwrap();
                            released_rx.recv().unwrap();
                        }
                    });
                    parked_rx.recv().unwrap();
                    SEM.release(1); // wakes the registrant's waiter
                    released_tx.send(()).unwrap();
                    registrant.await.unwrap();
                    // More reads than the bound, each of which would overtake the woken waiter.
                    for _ in 0..4 {
                        assert_eq!(SEM.load_permits(), Some(1));
                    }
                    let acq = acquire.lock().unwrap().take().unwrap();
                    acq.await.unwrap();
                    SEM.release(1);
                });
            },
            None,
        );
    }

    /// A woken waiter whose `Acquire` is dropped passes its wake on, but that is not a release: it
    /// does not overtake the other woken waiters. Here `b` is passed once only before the last
    /// release, so that release does not grant it at the default bound of 2.
    #[test_log::test]
    fn dropped_woken_acquire_does_not_overtake_the_other_woken_waiters() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b);
                    s.release(1); // wakes a
                    s.try_acquire(1).unwrap();
                    s.release(1); // passes a once, and wakes b
                    drop(a); // passes the wake on, to nobody
                    s.try_acquire(1).unwrap();
                    s.release(1); // passes b once
                    assert_eq!(s.available_permits(), 1, "b was granted: the drop overtook it");
                    ready!(b);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// Giving a reservation up is not a release either: it does not overtake the woken waiters.
    /// Here `r` is passed once only, by the shared release that wakes the writer, before the
    /// writer gives the reservation up, so the stand-in does not grant it at the default bound of
    /// 2.
    #[test_log::test]
    fn cancelled_reservation_does_not_overtake_the_woken_waiters() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(N);
                    s.acquire(N).await.unwrap();
                    let mut r = Box::pin(s.acquire(1));
                    pending!(r);
                    s.release(N); // wakes r
                    s.try_acquire(1).unwrap(); // a reader barges in
                    let mut w = Box::pin(s.acquire_reserving(RESERVE_AT, N));
                    pending!(w); // reserves
                    s.release_without_waking(1); // passes r once, and wakes w
                    drop(w); // gives the reservation up
                    assert_eq!(
                        s.available_permits(),
                        N,
                        "the cancel overtook r, and the stand-in granted it"
                    );
                    ready!(r);
                    s.release_without_waking(1);
                });
            },
            None,
        );
    }

    /// Nor is the wake of the waiters that the leftover permits of the holder of the reservation
    /// let in, when it takes its permits.
    #[test_log::test]
    fn holder_leftovers_do_not_overtake_the_woken_waiters() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(4);
                    s.acquire(4).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    s.release(1); // wakes a
                    let mut w = Box::pin(s.acquire_reserving(1, 2));
                    pending!(w); // reserves
                    s.release(3); // passes a once, and wakes w
                    ready!(w); // takes 2 of the 4
                    assert_eq!(
                        s.available_permits(),
                        2,
                        "the leftovers overtook a, and the stand-in granted it"
                    );
                    ready!(a);
                    s.release(2);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// But a reservation that its request gives up lets the stand-in grant a woken waiter that
    /// starves, which the reservation kept out at the releases that overtook it, as a release
    /// would: nothing else might ever release.
    #[test_log::test]
    fn cancelled_reservation_lets_the_stand_in_grant_a_starving_waiter() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(N);
                    s.acquire(N).await.unwrap();
                    let mut r = Box::pin(s.acquire(1));
                    pending!(r);
                    s.release(N); // wakes r
                    s.try_acquire(2).unwrap(); // two readers barge in
                    let mut w = Box::pin(s.acquire_reserving(RESERVE_AT, N));
                    pending!(w); // reserves
                    s.release_without_waking(1); // passes r once
                    s.release_without_waking(1); // twice: r starves, but the reservation keeps it out
                    drop(w); // gives the reservation up: the stand-in grants r
                    assert_eq!(s.available_permits(), N - 1, "the cancel did not grant r");
                    ready!(r);
                    s.release_without_waking(1);
                });
            },
            None,
        );
    }

    /// And so does the holder of the reservation that takes its permits, from the permits that it
    /// leaves free.
    #[test_log::test]
    fn holder_leftovers_let_the_stand_in_grant_a_starving_waiter() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(4);
                    s.acquire(4).await.unwrap();
                    let mut r = Box::pin(s.acquire(1));
                    pending!(r);
                    s.release(1); // wakes r
                    let mut w = Box::pin(s.acquire_reserving(1, 2));
                    pending!(w); // reserves, and waits for another permit
                    s.release_without_waking(1); // passes r once, and wakes w
                    s.release_without_waking(1); // twice: r starves, but the reservation keeps it out
                    ready!(w); // takes 2 of the 3 free permits: the stand-in grants r the last one
                    assert_eq!(
                        s.available_permits(),
                        0,
                        "the holder's leftovers did not let the stand-in grant r"
                    );
                    ready!(r);
                    s.release(2);
                    s.release(1);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// And so does a woken `Acquire` that is dropped before it tried, which passes its wake on: here
    /// the pass-on wakes `a`, which has lost two races, so the stand-in grants it.
    #[test_log::test]
    fn passed_on_wake_lets_the_stand_in_grant_a_waiter_that_lost_bound_races() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b);
                    s.release(1); // wakes a
                    s.try_acquire(1).unwrap();
                    pending!(a); // lost, so it parks again, behind b
                    s.release(1); // wakes b
                    s.try_acquire(1).unwrap();
                    pending!(b); // lost, so it parks again, behind a
                    s.release(1); // wakes a
                    s.try_acquire(1).unwrap();
                    pending!(a); // lost twice, and parks again, behind b
                    s.release(1); // wakes b, which has lost once
                    drop(b); // passes the wake on: wakes a, which starves
                    assert_eq!(s.available_permits(), 0, "the passed-on wake did not grant a");
                    ready!(a);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// The same when the pass-on drops a reservation whose task has finished, which kept a woken
    /// waiter that starves out at the releases that overtook it.
    #[test_log::test]
    fn passed_on_wake_lets_the_stand_in_grant_a_starving_waiter() {
        use std::pin::Pin;
        type Stash = Arc<Mutex<Option<Pin<Box<Acquire<'static>>>>>>;
        shuttle::lazy_static! {
            static ref SEM: BatchSemaphore = BatchSemaphore::new(3, Fairness::ParkingLot);
        }
        check_dfs(
            || {
                future::block_on(async {
                    SEM.acquire(3).await.unwrap();
                    let mut r = Box::pin(SEM.acquire(1));
                    pending!(r);
                    let mut d = Box::pin(SEM.acquire(1));
                    pending!(d);
                    SEM.release(2); // wakes r and d
                    SEM.try_acquire(1).unwrap();
                    let stash: Stash = Arc::new(Mutex::new(None));
                    let (reserved_tx, reserved_rx) = shuttle::sync::mpsc::channel::<()>();
                    let (finish_tx, finish_rx) = shuttle::sync::mpsc::channel::<()>();
                    let registrant = future::spawn({
                        let stash = Arc::clone(&stash);
                        async move {
                            let mut reserve = Box::pin(SEM.acquire_reserving(1, 3));
                            pending!(reserve); // one permit is free: reserves
                            *stash.lock().unwrap() = Some(reserve);
                            reserved_tx.send(()).unwrap();
                            finish_rx.recv().unwrap();
                        }
                    });
                    reserved_rx.recv().unwrap();
                    SEM.release_without_waking(1); // passes r and d once
                    SEM.release_without_waking(1); // twice: they starve, but the reservation keeps them out
                    finish_tx.send(()).unwrap();
                    registrant.await.unwrap(); // the holder's task has finished
                    drop(d); // passes the wake on, which drops the stale reservation: grants r
                    assert_eq!(
                        SEM.available_permits(),
                        2,
                        "the passed-on wake did not let the stand-in grant r"
                    );
                    ready!(r);
                    SEM.release(1);
                    drop(stash.lock().unwrap().take());
                });
            },
            None,
        );
    }

    /// A fair release runs the stand-in too: a woken waiter that two fair releases have passed is
    /// granted at the second, at the default bound of 2. A fair release that lets nobody in is a
    /// plain release, so without the stand-in, a task that takes and releases the lock in a loop
    /// with `release_fair`, as with `MutexGuard::unlock_fair`, would starve a woken waiter (see
    /// `fair_relock_loop_cannot_starve_a_woken_waiter`).
    #[test_log::test]
    fn fair_release_grants_a_starving_woken_waiter() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    s.release(1); // wakes a
                    s.try_acquire(1).unwrap();
                    s.release_fair(1); // passes a once: nobody is parked, so it hands nothing off
                    assert_eq!(s.available_permits(), 1);
                    s.try_acquire(1).unwrap();
                    s.release_fair(1); // passes a twice: the stand-in grants it
                    assert_eq!(s.available_permits(), 0, "the fair release ran no stand-in");
                    ready!(a);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// A woken waiter that starves tries at every release, as at a read (see
    /// `read_parks_a_starving_woken_waiter_that_cannot_take_its_permits`), so one whose permits a
    /// release does not leave free lost the race, and parks again, at the back. Here a fair
    /// release hands the only permit to `p`, which is parked, as `MutexGuard::unlock_fair` hands the
    /// lock to the thread at the front, and the next fair release hands it to `w`. If `w` stayed
    /// woken instead, no fair release would ever leave the permit free for it.
    #[test_log::test]
    fn starving_woken_waiter_that_a_fair_release_passes_parks_again() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut w = Box::pin(s.acquire(1));
                    pending!(w);
                    s.release(1); // wakes w
                    s.try_acquire(1).unwrap();
                    s.release(1); // passes w once
                    s.try_acquire(1).unwrap();
                    let mut p = Box::pin(s.acquire(1));
                    pending!(p);
                    s.release_fair(1); // passes w twice: it tries, and loses to p, which is handed the permit
                    assert_eq!(s.available_permits(), 0, "the fair release did not hand p the permit");
                    assert!(s.has_waiters(), "w did not park again");
                    pending!(w); // parked, so it does not try
                    ready!(p);
                    let mut q = Box::pin(s.acquire(1));
                    pending!(q); // parks behind w
                    s.release_fair(1); // hands w the permit
                    pending!(q);
                    ready!(w);
                    s.release_fair(1);
                    ready!(q);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// A release grants every starving woken waiter whose permits are free, not only the first.
    #[test_log::test]
    fn release_grants_every_starving_waiter_that_fits() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(2);
                    s.acquire(2).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b);
                    s.release(2); // wakes a and b
                    s.try_acquire(2).unwrap();
                    s.release(2); // passes both once
                    s.try_acquire(2).unwrap();
                    s.release(2); // twice: grants both
                    assert_eq!(s.available_permits(), 0, "the stand-in did not grant both");
                    ready!(a);
                    ready!(b);
                    s.release(1);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// The stand-in grants the oldest request first, the one that parked first, rather than the
    /// one that a release woke first. Here `b` was woken before `a`, both have been overtaken
    /// twice, and a release has one permit for them. It does so also on a semaphore that
    /// `const_new` made, as the locks of `shuttle-parking_lot` are.
    #[test_log::test]
    fn stand_in_grants_the_oldest_request_first() {
        for constant in [false, true] {
            check_dfs(
                move || {
                    future::block_on(async move {
                        let s = if constant {
                            BatchSemaphore::const_new(1, Fairness::ParkingLot)
                        } else {
                            sem(1)
                        };
                        stand_in_grants_the_oldest_request_first_on(s).await;
                    });
                },
                None,
            );
        }
    }

    async fn stand_in_grants_the_oldest_request_first_on(s: BatchSemaphore) {
        s.acquire(1).await.unwrap();
        let mut a = Box::pin(s.acquire(1));
        pending!(a);
        let mut b = Box::pin(s.acquire(1));
        pending!(b);

        s.release(1); // wakes a
        s.try_acquire(1).unwrap();
        pending!(a); // a lost once, and parks again behind b
        s.release(1); // wakes b
        s.try_acquire(1).unwrap();
        s.release(1); // passes b once, and wakes a
        s.try_acquire(1).unwrap();
        s.release(1); // passes both: each was overtaken twice
        assert_eq!(s.available_permits(), 0);
        assert!(s.has_waiters(), "b did not park again");
        ready!(a);
        pending!(b); // lost at the release, and parked again
        s.release(1); // wakes b, which was overtaken three times
        ready!(b);
        s.release(1);
    }

    /// Once the stand-in has handed a starving writer the reservation, it grants nothing more:
    /// the reservation keeps the free permits for the writer, so a starving reader behind it
    /// loses, and parks again. Here both have been overtaken at least twice, and the writer is the
    /// older request.
    #[test_log::test]
    fn stand_in_grants_nothing_behind_a_reservation_it_hands_over() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(N);
                    s.acquire(N).await.unwrap();
                    let mut w = Box::pin(s.acquire_reserving(RESERVE_AT, N));
                    pending!(w);
                    let mut r = Box::pin(s.acquire(1));
                    pending!(r);

                    s.release(N); // wakes w only
                    s.try_acquire(N).unwrap();
                    s.release(N); // passes w once, and wakes r
                    s.try_acquire(N).unwrap();
                    // Passes w twice and r once: w starves, but its permits are not free, so it
                    // parks again.
                    s.release(1);
                    s.try_acquire(1).unwrap();
                    // Wakes w, which is handed the reservation, and r, which starves too, parks
                    // again.
                    s.release(N - 1);
                    assert_eq!(s.available_permits(), 0);
                    assert!(s.has_waiters(), "r did not park again");
                    pending!(r); // lost to the reservation
                    s.release_without_waking(1); // the last permit: wakes w
                    ready!(w);
                    s.release(N); // wakes r, which has been overtaken three times
                    ready!(r);
                    s.release_without_waking(1);
                });
            },
            None,
        );
    }

    /// The holder of the reservation takes its permits as soon as they are free, and the waiters
    /// that the rest of the free permits let in are woken then: nothing else would wake them. Here
    /// another task waits behind the reservation, in some schedules.
    #[test_log::test]
    fn holder_that_takes_its_permits_wakes_the_waiters_that_the_rest_let_in() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = Arc::new(sem(4));
                    s.acquire(3).await.unwrap();
                    let mut reserve = Box::pin(s.acquire_reserving(1, 2));
                    pending!(reserve); // reserves: one permit is free
                    let other = future::spawn({
                        let s = Arc::clone(&s);
                        async move {
                            s.acquire(1).await.unwrap();
                            s.release(1);
                        }
                    });
                    future::yield_now().await; // lets `other` park behind the reservation
                    s.release(3);
                    reserve.await.unwrap(); // takes 2 of the 4
                    other.await.unwrap();
                    s.release(2);
                });
            },
            None,
        );
    }

    /// The waiters that the leftover permits let in are woken as a plain release wakes them, and
    /// are not handed the permits.
    #[test_log::test]
    fn holder_wakes_the_waiters_that_its_leftovers_let_in_without_hand_off() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(4);
                    s.acquire(3).await.unwrap();
                    let mut reserve = Box::pin(s.acquire_reserving(1, 2));
                    pending!(reserve); // reserves
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b); // parks behind the reservation
                    s.release(3);
                    ready!(reserve); // takes 2 of the 4, and wakes b
                    assert!(!s.has_waiters(), "the leftovers did not wake b");
                    assert_eq!(s.available_permits(), 2, "the leftovers were handed off");
                    ready!(b);
                    s.release(3);
                });
            },
            None,
        );
    }

    /// A reservation that its request gives up wakes the waiters that the free permits let in, as
    /// a `parking_lot` writer that gives up waiting for the readers clears `WRITER_BIT` and wakes
    /// the threads that it lets in, without a hand-off.
    #[test_log::test]
    fn dropped_reservation_wakes_without_hand_off() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(2);
                    s.acquire(1).await.unwrap();
                    let mut reserve = Box::pin(s.acquire_reserving(1, 2));
                    pending!(reserve); // reserves
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b); // parks behind the reservation
                    drop(reserve);
                    assert!(!s.has_waiters(), "the cancel did not wake b");
                    assert_eq!(s.available_permits(), 1, "the cancel handed the permit off");
                    ready!(b);
                    s.release(2);
                });
            },
            None,
        );
    }

    /// A release stops waking waiters once no permit is free (`FilterOp::Stop`), also before a
    /// waiter that could make progress without one: here a reserving acquire with `min_permits`
    /// 0, which parked because another request held the reservation.
    #[test_log::test]
    fn release_stops_at_the_last_free_permit() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(2);
                    s.acquire(1).await.unwrap(); // one free
                    let mut w = Box::pin(s.acquire_reserving(1, 2));
                    pending!(w); // reserves
                    let mut x = Box::pin(s.acquire(1));
                    pending!(x); // parks behind the reservation
                    let mut z = Box::pin(s.acquire_reserving(0, 1));
                    pending!(z); // parks: the reservation is held
                    drop(w); // wakes x, which uses up the free permit
                    assert!(s.has_waiters(), "the walk went past the last free permit to z");
                    ready!(x);
                    s.release(1); // x's permit
                    drop(z);
                    s.release(1); // the main task's permit
                });
            },
            None,
        );
    }

    /// A waiter that a release passes over keeps its place, ahead of the waiters that the release
    /// did not reach, as `unpark_filter` leaves a thread that it skips where it is. Here the queue
    /// is `[a(2), b(1), c(1)]`: a release of one permit passes over `a`, wakes `b`, and stops, and
    /// the next release of two wakes `a`, which uses both, and not `c`. In an `RwLock`, whose
    /// permits never run out at a skip, this cannot happen, so the filter test does not show it.
    #[test_log::test]
    fn skipped_waiter_keeps_its_place() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(3);
                    s.acquire(3).await.unwrap();
                    let mut a = Box::pin(s.acquire(2));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b);
                    let mut c = Box::pin(s.acquire(1));
                    pending!(c);
                    s.release(1); // passes over a, wakes b, and stops: c is not reached
                    ready!(b);
                    s.release(2); // the queue is [a, c]: wakes a, which uses both permits
                    pending!(c); // still parked
                    ready!(a);
                    s.release(2);
                    s.release(1);
                    ready!(c);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// The holder of the reservation that takes all of the free permits wakes nobody, not even a
    /// parked reserving request that needs no free permit to reserve the semaphore, as a
    /// `parking_lot` writer that takes the lock once the readers have left unparks nobody.
    #[test_log::test]
    fn holder_that_takes_every_free_permit_wakes_nobody() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(2);
                    s.acquire(1).await.unwrap();
                    let mut w = Box::pin(s.acquire_reserving(1, 2));
                    pending!(w); // reserves
                    let mut z = Box::pin(s.acquire_reserving(0, 1));
                    pending!(z); // parks: the reservation is held
                    s.release(1); // wakes w
                    ready!(w); // takes both permits
                    assert!(s.has_waiters(), "the holder's take woke z");
                    pending!(z);
                    s.release(2); // wakes z
                    ready!(z);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// A parked request whose `Acquire` is dropped leaves the queue and wakes nobody, as a parked
    /// `parking_lot` thread whose timed lock times out unparks nobody: here `b`, while `a` is
    /// woken and the permit is free, so `c` stays parked.
    #[test_log::test]
    fn dropped_parked_acquire_wakes_nobody() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b);
                    let mut c = Box::pin(s.acquire(1));
                    pending!(c);
                    s.release(1); // wakes a; the permit is free
                    drop(b);
                    assert!(s.has_waiters(), "dropping b woke c");
                    pending!(c);
                    ready!(a);
                    s.release(1); // wakes c
                    ready!(c);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// The same for a parked request of a task that has finished, which leaves the queue when
    /// another task polls its `Acquire`, and then tries like a new request (see
    /// `release_passes_over_a_parked_waiter_of_a_finished_task`).
    #[test_log::test]
    fn parked_waiter_of_a_finished_task_leaves_the_queue_waking_nobody() {
        shuttle::lazy_static! {
            static ref SEM: BatchSemaphore = BatchSemaphore::new(1, Fairness::ParkingLot);
        }
        check_dfs(
            || {
                future::block_on(async {
                    SEM.acquire(1).await.unwrap();
                    let mut a = Box::pin(SEM.acquire(1));
                    pending!(a);
                    let stash = Arc::new(Mutex::new(None));
                    let registrant = future::spawn({
                        let stash = Arc::clone(&stash);
                        async move {
                            let mut acquire = Box::pin(SEM.acquire(1));
                            pending!(acquire);
                            *stash.lock().unwrap() = Some(acquire);
                        }
                    });
                    registrant.await.unwrap(); // parked behind a, and its task has finished
                    let mut c = Box::pin(SEM.acquire(1));
                    pending!(c);
                    SEM.release(1); // wakes a; the permit is free
                    let mut abandoned = stash.lock().unwrap().take().unwrap();
                    ready!(abandoned); // leaves the queue, and takes the free permit
                    assert!(SEM.has_waiters(), "the abandoned request woke c when it left the queue");
                    pending!(a); // lost, and parks again, behind c
                    SEM.release(1); // wakes c
                    ready!(c);
                    SEM.release(1); // wakes a
                    ready!(a);
                    SEM.release(1);
                });
            },
            None,
        );
    }

    /// A fair release that drops a reservation whose task has finished hands the permits to the
    /// waiters that the reservation kept out, as it hands them off otherwise.
    #[test_log::test]
    fn fair_release_that_drops_a_stale_reservation_hands_off() {
        shuttle::lazy_static! {
            static ref SEM: BatchSemaphore = BatchSemaphore::new(2, Fairness::ParkingLot);
        }
        check_dfs(
            || {
                future::block_on(async {
                    SEM.acquire(1).await.unwrap();
                    let stash = Arc::new(Mutex::new(None));
                    let registrant = future::spawn({
                        let stash = Arc::clone(&stash);
                        async move {
                            let mut reserve = Box::pin(SEM.acquire_reserving(1, 2));
                            pending!(reserve); // reserves
                            *stash.lock().unwrap() = Some(reserve);
                        }
                    });
                    registrant.await.unwrap(); // the holder's task has finished
                    let mut b = Box::pin(SEM.acquire(1));
                    pending!(b); // parks behind the stale reservation
                    SEM.release_fair(1); // drops the stale reservation, and hands b a permit
                    assert_eq!(SEM.available_permits(), 1, "the fair release did not hand b off");
                    ready!(b);
                    SEM.release(1);
                    drop(stash.lock().unwrap().take());
                });
            },
            None,
        );
    }

    /// An `Acquire` that a fair release granted, and that is dropped before its task polls it,
    /// gives the permits back with a plain release, which wakes the next waiter, and hands
    /// nothing off.
    #[test_log::test]
    fn dropped_granted_acquire_releases_without_hand_off() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b);
                    s.release_fair(1); // grants a
                    drop(a); // releases its permit: wakes b
                    assert!(!s.has_waiters(), "b was not woken");
                    assert_eq!(s.available_permits(), 1, "the drop handed the permit off");
                    ready!(b);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// A release that takes waiters out of the queue is after the requests that parked there, as
    /// an unlock that unparks threads is after the threads that parked. Task 1 releases another
    /// semaphore first, so that its clock has an event of its own when it parks. When the main
    /// task's fair release hands it the permit, the main task is after it.
    #[test_log::test]
    fn release_that_unparks_is_after_the_parked_requests() {
        static HANDED_OFF: AtomicUsize = AtomicUsize::new(0);
        check_dfs(
            || {
                let s = Arc::new(sem(1));
                s.acquire_blocking(1).unwrap();
                let waiter = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || {
                        assert_eq!(me(), 1);
                        BatchSemaphore::new(0, Fairness::Unfair).release(1);
                        s.acquire_blocking(1).unwrap();
                        s.release(1);
                    })
                };
                s.release_fair(1);
                // No scheduling point since the release, so only a hand-off took the permit.
                if s.available_permits() == 0 {
                    HANDED_OFF.fetch_add(1, Ordering::SeqCst);
                    check_clock(|i, c| (i != 1) || (c > 0));
                }
                waiter.join().unwrap();
            },
            None,
        );
        assert!(HANDED_OFF.load(Ordering::SeqCst) > 0);
    }

    /// A waiter that a release grants its permits, by a hand-off or, at bound 0, by the stand-in,
    /// is causally after the release, as one that takes the permits itself is (see
    /// `batch_semaphore_clock_1`).
    #[test_log::test]
    fn granted_waiter_is_after_the_release() {
        for bound in [0, 2] {
            for fair in [false, true] {
                check_dfs_with_bound(bound, move || {
                    let s = Arc::new(sem(0));
                    let releaser = {
                        let s = Arc::clone(&s);
                        thread::spawn(move || {
                            assert_eq!(me(), 1);
                            if fair {
                                s.release_fair(1);
                            } else {
                                s.release(1);
                            }
                        })
                    };
                    let waiter = thread::spawn(move || {
                        assert_eq!(me(), 2);
                        s.acquire_blocking(1).unwrap();
                        check_clock(|i, c| (i != 1) || (c > 0));
                    });
                    releaser.join().unwrap();
                    waiter.join().unwrap();
                });
            }
        }
    }

    /// The same when the waiter takes the permit of an older release, which a ParkingLot
    /// semaphore can leave free while waiters are parked: a waiter that a release wakes or grants
    /// is causally after that release, as a thread that an unlock unparks is after the unlock,
    /// whichever permits it takes. Here the main task frees a permit with `release_without_waking`,
    /// which wakes nobody, and then lets the waiter in with `release` or `release_fair`.
    /// `RELEASING` records the main task's clock right before that release, with no scheduling
    /// point between them other than the release's own.
    #[test_log::test]
    fn waiter_let_in_on_the_permit_of_an_older_release_is_after_the_release() {
        static RELEASING: AtomicUsize = AtomicUsize::new(0);
        for bound in [0, 2] {
            for fair in [false, true] {
                check_dfs_with_bound(bound, move || {
                    RELEASING.store(usize::MAX, Ordering::SeqCst);
                    let s = Arc::new(sem(2));
                    s.try_acquire(2).unwrap();
                    let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
                    let waiter = {
                        let s = Arc::clone(&s);
                        thread::spawn(move || {
                            future::block_on(async move {
                                let mut acquire = Box::pin(s.acquire(1));
                                pending!(acquire);
                                parked_tx.send(()).unwrap();
                                acquire.await.unwrap();
                                assert!(
                                    clock_entry(0) > RELEASING.load(Ordering::SeqCst),
                                    "the waiter is not after the release that let it in"
                                );
                                s.release(1);
                            })
                        })
                    };
                    parked_rx.recv().unwrap();
                    s.release_without_waking(1); // frees a permit, and wakes nobody
                    RELEASING.store(clock_entry(0), Ordering::SeqCst);
                    if fair {
                        s.release_fair(1); // hands the waiter the older permit
                    } else {
                        s.release(1); // wakes the waiter, which takes the older permit
                    }
                    waiter.join().unwrap();
                });
            }
        }
    }

    /// A waiter that a release grants the permit of an older release of another task is
    /// causally after that older release too, as one that takes the permit itself is: the grant
    /// joins the clocks of the permits that it takes, as `take_permits` does. Here task 1 frees a
    /// permit with `release_without_waking`, which wakes nobody, and the main task, which is not
    /// after that release, hands the parked waiter, task 2, the oldest free permit with
    /// `release_fair`. `OLDER` records whether task 1's permit was free then: the main task reads
    /// task 1's flag right after its release, with no scheduling point between them.
    #[test_log::test]
    fn waiter_granted_the_permit_of_another_task_is_after_that_release() {
        static RELEASING: AtomicUsize = AtomicUsize::new(0);
        static FREED: AtomicBool = AtomicBool::new(false);
        static OLDER: AtomicBool = AtomicBool::new(false);
        static CHECKED: AtomicUsize = AtomicUsize::new(0);
        check_dfs(
            || {
                RELEASING.store(usize::MAX, Ordering::SeqCst);
                FREED.store(false, Ordering::SeqCst);
                OLDER.store(false, Ordering::SeqCst);
                let s = Arc::new(sem(2));
                s.try_acquire(1).unwrap();
                let (held_tx, held_rx) = shuttle::sync::mpsc::channel::<()>();
                let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
                let releaser = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || {
                        assert_eq!(me(), 1);
                        s.try_acquire(1).unwrap();
                        held_tx.send(()).unwrap();
                        go_rx.recv().unwrap();
                        BatchSemaphore::new(0, Fairness::Unfair).release(1);
                        RELEASING.store(clock_entry(1), Ordering::SeqCst);
                        s.release_without_waking(1); // frees a permit, and wakes nobody
                        FREED.store(true, Ordering::SeqCst);
                    })
                };
                held_rx.recv().unwrap();
                let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
                let waiter = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || {
                        assert_eq!(me(), 2);
                        future::block_on(async {
                            let mut acquire = Box::pin(s.acquire(1));
                            pending!(acquire);
                            parked_tx.send(()).unwrap();
                            acquire.await.unwrap();
                        });
                        if OLDER.load(Ordering::SeqCst) {
                            CHECKED.fetch_add(1, Ordering::SeqCst);
                            assert!(
                                clock_entry(1) > RELEASING.load(Ordering::SeqCst),
                                "the waiter is not after the release whose permit it was granted"
                            );
                        }
                        s.release(1);
                    })
                };
                parked_rx.recv().unwrap();
                go_tx.send(()).unwrap();
                s.release_fair(1); // hands the waiter the oldest free permit
                OLDER.store(FREED.load(Ordering::SeqCst), Ordering::SeqCst);
                releaser.join().unwrap();
                waiter.join().unwrap();
            },
            None,
        );
        assert!(
            CHECKED.load(Ordering::SeqCst) > 0,
            "no schedule freed task 1's permit first"
        );
    }

    /// A fair release of one permit lets in a waiter for two, which takes the permit that was
    /// already free too. A `try_acquire` that fails after the grant would have succeeded before the
    /// release, so it is after the release. `RELEASED` records the main task's clock right after
    /// the release, with no scheduling point between them.
    #[test_log::test]
    fn failed_try_acquire_is_after_a_grant_that_took_freed_permits() {
        static RELEASED: AtomicUsize = AtomicUsize::new(0);
        static CHECKED: AtomicUsize = AtomicUsize::new(0);
        check_dfs(
            || {
                RELEASED.store(0, Ordering::SeqCst);
                let s = Arc::new(sem(2));
                s.try_acquire(1).unwrap();
                let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
                let waiter = {
                    let s = Arc::clone(&s);
                    future::spawn(async move {
                        let mut a = Box::pin(s.acquire(2));
                        pending!(a);
                        parked_tx.send(()).unwrap();
                        a.await.unwrap();
                        s.release(2);
                    })
                };
                parked_rx.recv().unwrap();
                let other = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || match s.try_acquire(1) {
                        Ok(()) => s.release(1),
                        Err(_) => {
                            let released = RELEASED.load(Ordering::SeqCst);
                            if released > 0 {
                                CHECKED.fetch_add(1, Ordering::SeqCst);
                                assert!(clock_entry(0) >= released, "the failed try is not after the release");
                            }
                        }
                    })
                };
                s.release_fair(1);
                RELEASED.store(clock_entry(0), Ordering::SeqCst);
                other.join().unwrap();
                future::block_on(waiter).unwrap();
            },
            None,
        );
        assert!(CHECKED.load(Ordering::SeqCst) > 0);
    }

    /// Task 1 holds a permit, and gives it back with `release_without_waking`, which wakes nobody,
    /// so the reserving waiter stays parked. The main task's fair release then hands the waiter
    /// the reservation, and task 1's `try_acquire` fails, which it would not have done before the
    /// release. So the failure is after the release.
    #[test_log::test]
    fn failed_try_acquire_is_after_the_release_that_handed_the_reservation_over() {
        static RELEASED: AtomicUsize = AtomicUsize::new(0);
        static CHECKED: AtomicUsize = AtomicUsize::new(0);
        check_dfs(
            || {
                RELEASED.store(0, Ordering::SeqCst);
                let s = Arc::new(sem(3));
                s.try_acquire(2).unwrap();
                let (held_tx, held_rx) = shuttle::sync::mpsc::channel::<()>();
                let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
                let (parked_tx2, parked_rx2) = shuttle::sync::mpsc::channel::<()>();
                let (done_tx, done_rx) = shuttle::sync::mpsc::channel::<()>();
                let other = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || {
                        s.try_acquire(1).unwrap();
                        held_tx.send(()).unwrap();
                        parked_rx2.recv().unwrap();
                        s.release_without_waking(1);
                        match s.try_acquire(1) {
                            Ok(()) => s.release(1),
                            Err(_) => {
                                let released = RELEASED.load(Ordering::SeqCst);
                                assert!(released > 0, "refused before the hand-over");
                                CHECKED.fetch_add(1, Ordering::SeqCst);
                                assert!(clock_entry(0) >= released, "the failed try is not after the release");
                            }
                        }
                        done_tx.send(()).unwrap();
                    })
                };
                held_rx.recv().unwrap();
                let waiter = {
                    let s = Arc::clone(&s);
                    future::spawn(async move {
                        let mut w = Box::pin(s.acquire_reserving(1, 3));
                        pending!(w); // parks: no permit is free
                        parked_tx.send(()).unwrap();
                        parked_tx2.send(()).unwrap();
                        w.await.unwrap();
                        s.release(3);
                    })
                };
                parked_rx.recv().unwrap();
                s.release_fair(1); // hands the waiter the reservation
                RELEASED.store(clock_entry(0), Ordering::SeqCst);
                done_rx.recv().unwrap();
                s.release(1);
                other.join().unwrap();
                future::block_on(waiter).unwrap();
            },
            None,
        );
        assert!(CHECKED.load(Ordering::SeqCst) > 0);
    }

    /// A `try_acquire` that a handed-over reservation refuses is after the request that holds the
    /// reservation, also when another task than the one that made the request polls it, as an
    /// `Acquire` can move between tasks. Here the main task makes the request after an event of its
    /// own, and task 1, which knows the main task's events only through the semaphore, polls it and
    /// releases the permits that it waits for with `release_fair`.
    #[test_log::test]
    fn failed_try_acquire_is_after_the_maker_of_a_moved_reservation() {
        use std::pin::Pin;
        type Stash = Arc<Mutex<Option<Pin<Box<Acquire<'static>>>>>>;
        static CHECKED: AtomicUsize = AtomicUsize::new(0);
        shuttle::lazy_static! {
            static ref SEM: BatchSemaphore = BatchSemaphore::new(N, Fairness::ParkingLot);
        }
        check_dfs(
            || {
                SEM.try_acquire(N).unwrap();
                let stash: Stash = Arc::new(Mutex::new(None));
                let made_at = Arc::new(AtomicUsize::new(0));
                let other = {
                    let (stash, made_at) = (Arc::clone(&stash), Arc::clone(&made_at));
                    thread::spawn(move || {
                        thread::yield_now();
                        // A `std` mutex, which adds no clock edge.
                        let Some(mut acquire) = stash.lock().unwrap().take() else {
                            return;
                        };
                        let made_at = made_at.load(Ordering::SeqCst);
                        future::block_on(async {
                            pending!(acquire); // parks, for this task
                        });
                        SEM.release_fair(UPG); // lets it reserve: it is handed the reservation
                        assert_eq!(SEM.try_acquire(1), Err(TryAcquireError::NoPermits));
                        CHECKED.fetch_add(1, Ordering::SeqCst);
                        assert!(
                            clock_entry(0) >= made_at,
                            "the failed try is not after the request's maker"
                        );
                        drop(acquire); // gives the reservation up
                    })
                };
                BatchSemaphore::new(0, Fairness::Unfair).release(1);
                made_at.store(clock_entry(0), Ordering::SeqCst);
                *stash.lock().unwrap() = Some(Box::pin(SEM.acquire_reserving(RESERVE_AT, N)));
                other.join().unwrap();
                drop(stash.lock().unwrap().take());
            },
            None,
        );
        assert!(CHECKED.load(Ordering::SeqCst) > 0, "no schedule moved the request");
    }

    /// A release is after a woken waiter's lost retry, which changed the woken waiters
    /// (`last_woken`), also a shared release that reads no queue, so that the stand-in's decisions
    /// are after the retries that they count. The waiter is task 1, which has an event of its own
    /// between its park and its retry. It waits for `go` before it retries, so the main task's
    /// `try_acquire` takes the permit first, and it loses.
    #[test_log::test]
    fn shared_release_is_after_a_lost_retry() {
        static CHECKED: AtomicUsize = AtomicUsize::new(0);
        check_dfs(
            || {
                let s = Arc::new(sem(1));
                s.try_acquire(1).unwrap();
                let lost = Arc::new(AtomicBool::new(false));
                let retried_at = Arc::new(AtomicUsize::new(0));
                let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
                let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
                let waiter = {
                    let (s, lost, retried_at) = (Arc::clone(&s), Arc::clone(&lost), Arc::clone(&retried_at));
                    thread::spawn(move || {
                        assert_eq!(me(), 1);
                        future::block_on(async move {
                            let mut acquire = Box::pin(s.acquire(1));
                            pending!(acquire);
                            parked_tx.send(()).unwrap();
                            BatchSemaphore::new(0, Fairness::Unfair).release(1);
                            retried_at.store(clock_entry(1), Ordering::SeqCst);
                            go_rx.recv().unwrap();
                            if futures::poll!(acquire.as_mut()).is_pending() {
                                lost.store(true, Ordering::SeqCst);
                                acquire.await.unwrap();
                            }
                            s.release(1);
                        })
                    })
                };
                parked_rx.recv().unwrap();
                s.release(1); // wakes the waiter
                s.try_acquire(1).unwrap(); // the waiter waits for `go`, so it has not tried yet
                go_tx.send(()).unwrap();
                s.release_without_waking(1);
                // No scheduling point since the release: if the retry lost, it was before it.
                if lost.load(Ordering::SeqCst) {
                    CHECKED.fetch_add(1, Ordering::SeqCst);
                    assert!(
                        clock_entry(1) >= retried_at.load(Ordering::SeqCst),
                        "the shared release is not after the lost retry"
                    );
                }
                if s.try_acquire(1).is_ok() {
                    s.release(1);
                }
                waiter.join().unwrap();
            },
            None,
        );
        assert!(CHECKED.load(Ordering::SeqCst) > 0);
    }

    /// As above, with a retry that wins, and a plain release by another task while nobody is
    /// parked, which reads no queue either.
    #[test_log::test]
    fn release_is_after_a_won_retry() {
        static CHECKED: AtomicUsize = AtomicUsize::new(0);
        check_dfs(
            || {
                let s = Arc::new(sem(2));
                s.try_acquire(2).unwrap();
                let won = Arc::new(AtomicBool::new(false));
                let retried_at = Arc::new(AtomicUsize::new(0));
                let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
                let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
                let waiter = {
                    let (s, won, retried_at) = (Arc::clone(&s), Arc::clone(&won), Arc::clone(&retried_at));
                    thread::spawn(move || {
                        assert_eq!(me(), 1);
                        future::block_on(async move {
                            let mut acquire = Box::pin(s.acquire(1));
                            pending!(acquire);
                            parked_tx.send(()).unwrap();
                            BatchSemaphore::new(0, Fairness::Unfair).release(1);
                            retried_at.store(clock_entry(1), Ordering::SeqCst);
                            go_rx.recv().unwrap();
                            acquire.await.unwrap();
                            won.store(true, Ordering::SeqCst);
                            s.release(1);
                        })
                    })
                };
                parked_rx.recv().unwrap();
                s.release(1); // wakes the waiter, and leaves one permit free
                go_tx.send(()).unwrap();
                s.release(1);
                if won.load(Ordering::SeqCst) {
                    CHECKED.fetch_add(1, Ordering::SeqCst);
                    assert!(
                        clock_entry(1) >= retried_at.load(Ordering::SeqCst),
                        "the release is not after the won retry"
                    );
                }
                waiter.join().unwrap();
            },
            None,
        );
        assert!(CHECKED.load(Ordering::SeqCst) > 0);
    }

    /// A read (`load_permits`) is after the woken waiters' clock (`last_woken`) too, and a read
    /// that overtakes them adds itself to it, as a release does, so that the stand-in's decision
    /// at a read is after the reads and releases that it counts. Here task 2's read overtakes the
    /// woken waiter once, after an event of its own, and then the main task's read grants it:
    /// that read must be after task 2's.
    #[test_log::test]
    fn read_that_lets_a_waiter_try_is_after_the_read_that_overtook_it() {
        static CHECKED: AtomicUsize = AtomicUsize::new(0);
        check_dfs(
            || {
                let s = Arc::new(sem(1));
                s.try_acquire(1).unwrap();
                let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
                let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
                let waiter = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || {
                        assert_eq!(me(), 1);
                        future::block_on(async move {
                            let mut acquire = Box::pin(s.acquire(1));
                            pending!(acquire);
                            parked_tx.send(()).unwrap();
                            go_rx.recv().unwrap();
                            acquire.await.unwrap();
                            s.release(1);
                        })
                    })
                };
                parked_rx.recv().unwrap();
                s.release(1); // wakes the waiter, which waits for `go`
                let read_at = Arc::new(AtomicUsize::new(0));
                let reader = {
                    let (s, read_at) = (Arc::clone(&s), Arc::clone(&read_at));
                    thread::spawn(move || {
                        assert_eq!(me(), 2);
                        BatchSemaphore::new(0, Fairness::Unfair).release(1);
                        read_at.store(clock_entry(2), Ordering::SeqCst);
                        s.load_permits();
                    })
                };
                // Only a read that finds the waiter overtaken once already grants it.
                if s.load_permits() == Some(0) {
                    CHECKED.fetch_add(1, Ordering::SeqCst);
                    let at = read_at.load(Ordering::SeqCst);
                    assert!(at > 0);
                    assert!(
                        clock_entry(2) >= at,
                        "the read is not after the read that overtook the waiter"
                    );
                }
                go_tx.send(()).unwrap();
                reader.join().unwrap();
                waiter.join().unwrap();
            },
            None,
        );
        assert!(CHECKED.load(Ordering::SeqCst) > 0);
    }

    /// A read that parks a woken waiter again adds itself to the changes of the queue
    /// (`last_queue_change`), so a `has_waiters` that sees the waiter is after that read.
    #[test_log::test]
    fn has_waiters_is_after_the_read_that_parked_the_waiter_again() {
        static CHECKED: AtomicUsize = AtomicUsize::new(0);
        check_dfs(
            || {
                let s = Arc::new(sem(1));
                s.try_acquire(1).unwrap();
                let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
                let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
                let waiter = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || {
                        assert_eq!(me(), 1);
                        future::block_on(async move {
                            let mut acquire = Box::pin(s.acquire(1));
                            pending!(acquire);
                            parked_tx.send(()).unwrap();
                            go_rx.recv().unwrap();
                            acquire.await.unwrap();
                            s.release(1);
                        })
                    })
                };
                parked_rx.recv().unwrap();
                s.release(1); // wakes the waiter, which waits for `go`
                s.try_acquire(1).unwrap(); // takes the permit back first
                let read_at = Arc::new(AtomicUsize::new(0));
                let reader = {
                    let (s, read_at) = (Arc::clone(&s), Arc::clone(&read_at));
                    thread::spawn(move || {
                        assert_eq!(me(), 2);
                        BatchSemaphore::new(0, Fairness::Unfair).release(1);
                        read_at.store(clock_entry(2), Ordering::SeqCst);
                        s.load_permits();
                        s.load_permits(); // the waiter tries, loses, and parks again
                    })
                };
                thread::yield_now();
                if s.has_waiters() {
                    CHECKED.fetch_add(1, Ordering::SeqCst);
                    let at = read_at.load(Ordering::SeqCst);
                    assert!(at > 0);
                    assert!(
                        clock_entry(2) >= at,
                        "has_waiters is not after the read that parked the waiter again"
                    );
                }
                go_tx.send(()).unwrap();
                s.release(1);
                reader.join().unwrap();
                waiter.join().unwrap();
            },
            None,
        );
        assert!(CHECKED.load(Ordering::SeqCst) > 0);
    }

    /// A read is after the woken waiters' clock even when no woken waiter is left: its result,
    /// which takes nothing, would differ if one were, as a read can grant one. Here task 1 drops
    /// its woken `Acquire`, after an event of its own, which empties the woken waiters, before
    /// the main task's second read, which would have granted it otherwise.
    #[test_log::test]
    fn read_is_after_the_drop_that_left_no_woken_waiter() {
        static CHECKED: AtomicUsize = AtomicUsize::new(0);
        check_dfs(
            || {
                let s = Arc::new(sem(1));
                s.try_acquire(1).unwrap();
                let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
                let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
                let dropped = Arc::new(AtomicBool::new(false));
                let dropped_at = Arc::new(AtomicUsize::new(0));
                let waiter = {
                    let (s, dropped, dropped_at) = (Arc::clone(&s), Arc::clone(&dropped), Arc::clone(&dropped_at));
                    thread::spawn(move || {
                        assert_eq!(me(), 1);
                        future::block_on(async move {
                            let mut acquire = Box::pin(s.acquire(1));
                            pending!(acquire);
                            parked_tx.send(()).unwrap();
                            go_rx.recv().unwrap();
                            BatchSemaphore::new(0, Fairness::Unfair).release(1);
                            dropped_at.store(clock_entry(1), Ordering::SeqCst);
                            drop(acquire); // woken, and never tried: passes the wake on
                            dropped.store(true, Ordering::SeqCst);
                        })
                    })
                };
                parked_rx.recv().unwrap();
                s.release(1); // wakes the waiter
                assert_eq!(s.load_permits(), Some(1)); // passes it once
                go_tx.send(()).unwrap();
                let read = s.load_permits(); // would grant it, if it were still woken
                if dropped.load(Ordering::SeqCst) {
                    assert_eq!(read, Some(1));
                    CHECKED.fetch_add(1, Ordering::SeqCst);
                    assert!(
                        clock_entry(1) >= dropped_at.load(Ordering::SeqCst),
                        "the read is not after the drop that left no woken waiter"
                    );
                }
                waiter.join().unwrap();
            },
            None,
        );
        assert!(CHECKED.load(Ordering::SeqCst) > 0);
    }

    /// A fair release counts its passes before it reads the queue, and a hand-off is not a wake,
    /// so when its stand-in grants a starving woken waiter, only the grant joins the parks that the
    /// release read into the woken waiters' clock (`last_woken`). A later release by another task,
    /// which reads no queue, must still be after them. Task 2 parks after an event of its own.
    #[test_log::test]
    fn release_is_after_the_parks_that_a_fair_release_read_before_a_stand_in_grant() {
        static CHECKED: AtomicUsize = AtomicUsize::new(0);
        check_dfs(
            || {
                let s = Arc::new(sem(5));
                s.try_acquire(4).unwrap();
                let (ready_tx, ready_rx) = shuttle::sync::mpsc::channel::<()>();
                let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
                let fair_done = Arc::new(AtomicBool::new(false));
                let read_the_park = Arc::new(AtomicBool::new(false));
                let parked = Arc::new(AtomicBool::new(false));
                let parked_at = Arc::new(AtomicUsize::new(0));
                let other = {
                    let (s, fair_done, read_the_park, parked_at) = (
                        Arc::clone(&s),
                        Arc::clone(&fair_done),
                        Arc::clone(&read_the_park),
                        Arc::clone(&parked_at),
                    );
                    thread::spawn(move || {
                        assert_eq!(me(), 1);
                        s.try_acquire(1).unwrap();
                        ready_tx.send(()).unwrap();
                        go_rx.recv().unwrap();
                        s.release(1);
                        if fair_done.load(Ordering::SeqCst) && read_the_park.load(Ordering::SeqCst) {
                            CHECKED.fetch_add(1, Ordering::SeqCst);
                            assert!(
                                clock_entry(2) >= parked_at.load(Ordering::SeqCst),
                                "the release is not after the park that the fair release read"
                            );
                        }
                    })
                };
                ready_rx.recv().unwrap();
                future::block_on(async {
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a); // parks
                    s.release(1); // wakes a
                    s.try_acquire(1).unwrap();
                    s.release(1); // passes a once
                    s.try_acquire(1).unwrap(); // no permit is free now
                    let parker = {
                        let (s, parked, parked_at) = (Arc::clone(&s), Arc::clone(&parked), Arc::clone(&parked_at));
                        thread::spawn(move || {
                            assert_eq!(me(), 2);
                            BatchSemaphore::new(0, Fairness::Unfair).release(1);
                            parked_at.store(clock_entry(2), Ordering::SeqCst);
                            future::block_on(async move {
                                let mut p = Box::pin(s.acquire(1));
                                if futures::poll!(p.as_mut()).is_pending() {
                                    parked.store(true, Ordering::SeqCst);
                                    p.await.unwrap();
                                }
                                s.release(1);
                            })
                        })
                    };
                    go_tx.send(()).unwrap();
                    // Passes a twice, hands p a permit, and the stand-in grants a the other.
                    s.release_fair(2);
                    read_the_park.store(parked.load(Ordering::SeqCst), Ordering::SeqCst);
                    fair_done.store(true, Ordering::SeqCst);
                    ready!(a);
                    s.release(1);
                    s.release(2);
                    parker.join().unwrap();
                });
                other.join().unwrap();
            },
            None,
        );
        assert!(CHECKED.load(Ordering::SeqCst) > 0);
    }

    /// A woken waiter whose `Acquire` is dropped before it tries passes its wake on, so that the
    /// waiter behind it is not stranded on a free permit. It passes it on as the plain release
    /// that woke it did: the next waiter is woken, and the permit stays free, so another request
    /// can take it first.
    #[test_log::test]
    fn dropped_woken_acquire_passes_the_wake_on() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b);
                    s.release(1); // wakes a
                    drop(a); // wakes b in its place
                    assert!(!s.has_waiters(), "b is still parked");
                    assert_eq!(s.available_permits(), 1, "the wake was passed on as a hand-off");
                    s.try_acquire(1).unwrap();
                    pending!(b); // lost, and parks again
                    s.release(1);
                    ready!(b);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// A woken acquire that is dropped while its task panics does not pass its wake on, as a
    /// release then does not wake anybody either (see `release_while_panicking_forgets_the_woken_waiters`),
    /// although the semaphore is not closed: here the waiter behind it stays parked on the free
    /// permit until the next release. Code that catches a panic and goes on using the lock can
    /// strand a waiter in the same way with a release while panicking.
    #[test_log::test]
    fn woken_acquire_dropped_while_panicking_passes_no_wake_on() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b);
                    s.release(1); // wakes a
                    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _a = a;
                        panic!("expected panic");
                    }));
                    assert!(panicked.is_err());
                    assert!(!s.is_closed());
                    assert_eq!(s.available_permits(), 1);
                    assert!(s.has_waiters(), "the drop passed the wake on to b");
                    pending!(b); // still parked: it does not try, although the permit is free
                    s.try_acquire(1).unwrap();
                    s.release(1); // wakes b
                    ready!(b);
                    s.release(1);
                });
            },
            None,
        );
    }

    /// The same across tasks: the first waiter's task drops its `Acquire` without trying, after
    /// a release may have woken it, and the second waiter must still get the permit.
    #[test_log::test]
    fn dropped_woken_acquire_of_another_task_passes_the_wake_on() {
        check_dfs(
            || {
                let s = Arc::new(sem(1));
                s.acquire_blocking(1).unwrap();
                let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
                let (released_tx, released_rx) = shuttle::sync::mpsc::channel::<()>();
                let first = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || {
                        future::block_on(async {
                            let mut acquire = Box::pin(s.acquire(1));
                            pending!(acquire);
                            parked_tx.send(()).unwrap();
                            released_rx.recv().unwrap();
                        });
                    })
                };
                parked_rx.recv().unwrap();
                let second = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || {
                        s.acquire_blocking(1).unwrap();
                        s.release(1);
                    })
                };
                s.release(1);
                released_tx.send(()).unwrap();
                first.join().unwrap();
                second.join().unwrap();
            },
            None,
        );
    }

    /// A task waits on two acquires at once, and a fair release grants one of them. The task must
    /// run to take the permit, although its other acquire is still queued.
    #[test_log::test]
    fn fair_release_to_a_task_with_another_queued_acquire() {
        check_dfs(
            || {
                let s = Arc::new(sem(0));
                let waiter = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || {
                        future::block_on(async {
                            let (one, two) = (s.acquire(1), s.acquire(2));
                            futures::pin_mut!(one, two);
                            match select(one, two).await {
                                Either::Left((result, _)) => result.unwrap(),
                                Either::Right((result, _)) => result.unwrap(),
                            }
                        })
                    })
                };
                s.release_fair(1);
                waiter.join().unwrap();
            },
            None,
        );
    }

    /// As above, with another task that takes the permit that is left after a fair release of two,
    /// which grants the acquire of one and passes over the acquire of two.
    #[test_log::test]
    fn later_acquire_leaves_a_granted_task_runnable() {
        check_dfs(
            || {
                let s = Arc::new(sem(0));
                let (queued_tx, queued_rx) = shuttle::sync::mpsc::channel();
                let waiter = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || {
                        future::block_on(async {
                            let (one, two) = (s.acquire(1), s.acquire(2));
                            futures::pin_mut!(one, two);
                            assert!(futures::poll!(one.as_mut()).is_pending());
                            assert!(futures::poll!(two.as_mut()).is_pending());
                            queued_tx.send(()).unwrap();
                            match select(one, two).await {
                                Either::Left((result, _)) => result.unwrap(),
                                Either::Right((result, _)) => result.unwrap(),
                            }
                        })
                    })
                };
                queued_rx.recv().unwrap();
                s.release_fair(2);
                s.try_acquire(1).unwrap();
                waiter.join().unwrap();
            },
            None,
        );
    }

    /// The order in which requests park decides which one a release wakes, so Shuttle explores
    /// every order: with the permit held and two waiters, either can be the first to get it.
    #[test_log::test]
    fn explores_every_park_order() {
        let firsts = Arc::new(Mutex::new(HashSet::new()));
        let firsts_clone = Arc::clone(&firsts);
        check_dfs(
            move || {
                let s = Arc::new(sem(1));
                s.acquire_blocking(1).unwrap();
                let order = Arc::new(Mutex::new(Vec::new()));
                let waiters = (0..2)
                    .map(|_| {
                        let (s, order) = (Arc::clone(&s), Arc::clone(&order));
                        thread::spawn(move || {
                            s.acquire_blocking(1).unwrap();
                            order.lock().unwrap().push(me());
                            s.release(1);
                        })
                    })
                    .collect::<Vec<_>>();
                s.release(1);
                for waiter in waiters {
                    waiter.join().unwrap();
                }
                firsts_clone.lock().unwrap().insert(order.lock().unwrap()[0]);
            },
            None,
        );
        assert_eq!(*firsts.lock().unwrap(), HashSet::from([1, 2]));
    }

    /// A `try_acquire_without_overtaking` that a parked waiter refuses is after that waiter joined
    /// the queue, as a failed `try_write` is after the thread that set `PARKED_BIT`. Task 1
    /// releases another semaphore first, so that its clock has an event of its own when it parks.
    #[test_log::test]
    fn failed_try_acquire_without_overtaking_is_after_the_parked_waiter() {
        static REFUSED: AtomicUsize = AtomicUsize::new(0);
        check_dfs(
            || {
                let s = Arc::new(sem(2));
                s.acquire_blocking(1).unwrap();
                let waiter = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || {
                        assert_eq!(me(), 1);
                        BatchSemaphore::new(0, Fairness::Unfair).release(1);
                        s.acquire_blocking(2).unwrap();
                        s.release(2);
                    })
                };
                let other = {
                    let s = Arc::clone(&s);
                    thread::spawn(move || {
                        assert_eq!(me(), 2);
                        // One permit is free while the main task holds the other, so only task 1
                        // can make this fail, by waiting for the permits or by holding them.
                        match s.try_acquire_without_overtaking(1) {
                            Ok(()) => s.release(1),
                            Err(_) => {
                                REFUSED.fetch_add(1, Ordering::SeqCst);
                                check_clock(|i, c| (i != 1) || (c > 0));
                            }
                        }
                    })
                };
                thread::yield_now();
                s.release(1);
                waiter.join().unwrap();
                other.join().unwrap();
            },
            None,
        );
        assert!(REFUSED.load(Ordering::SeqCst) > 0);
    }

    /// `release_fair` and `release_without_waking` are `release` on the other modes: the same
    /// schedules, with the same outcomes.
    #[test_log::test]
    fn other_modes_release_alike() {
        type Release = fn(&BatchSemaphore, usize);
        let releases: [Release; 3] = [
            BatchSemaphore::release,
            BatchSemaphore::release_fair,
            BatchSemaphore::release_without_waking,
        ];
        for fairness in [Fairness::StrictlyFair, Fairness::Unfair] {
            let runs = releases.map(|release| {
                let orders = Arc::new(Mutex::new(HashSet::new()));
                let orders_clone = Arc::clone(&orders);
                let executions = run_with_bound(DfsScheduler::new(None, false), 2, 1_000, move || {
                    let s = Arc::new(BatchSemaphore::new(1, fairness));
                    s.acquire_blocking(1).unwrap();
                    let order = Arc::new(Mutex::new(Vec::new()));
                    let waiters = (0..2)
                        .map(|_| {
                            let (s, order) = (Arc::clone(&s), Arc::clone(&order));
                            thread::spawn(move || {
                                s.acquire_blocking(1).unwrap();
                                order.lock().unwrap().push(me());
                                release(&s, 1);
                            })
                        })
                        .collect::<Vec<_>>();
                    release(&s, 1);
                    for waiter in waiters {
                        waiter.join().unwrap();
                    }
                    orders_clone.lock().unwrap().insert(order.lock().unwrap().clone());
                });
                let orders = orders.lock().unwrap().clone();
                (executions, orders)
            });
            assert_eq!(runs[0], runs[1], "{fairness:?}: release_fair");
            assert_eq!(runs[0], runs[2], "{fairness:?}: release_without_waking");
        }
    }

    /// Closing the semaphore wakes the woken waiters too, with the parked ones and the holder of
    /// the reservation, and the acquires of all of them fail.
    #[test_log::test]
    fn close_fails_woken_parked_and_reserving_acquires() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(2);
                    s.acquire(2).await.unwrap();
                    let mut woken = Box::pin(s.acquire(1));
                    pending!(woken);
                    let mut parked = Box::pin(s.acquire(1));
                    pending!(parked);
                    s.release(1); // wakes `woken` only
                    let mut reserving = Box::pin(s.acquire_reserving(1, 2));
                    pending!(reserving); // one permit is free, so it reserves the semaphore
                    assert!(s.has_waiters());
                    s.close();
                    for acquire in [&mut woken, &mut parked, &mut reserving] {
                        assert!(matches!(futures::poll!(acquire.as_mut()), Poll::Ready(Err(_))));
                    }
                });
            },
            None,
        );
    }

    /// A release while the task panics closes the semaphore, to model lock poisoning, and forgets
    /// its waiters without waking them: the woken ones too.
    #[test_log::test]
    fn release_while_panicking_forgets_the_woken_waiters() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(1);
                    s.acquire(1).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    let mut b = Box::pin(s.acquire(1));
                    pending!(b);
                    s.release(1); // wakes a
                    s.try_acquire(1).unwrap();

                    // A lock guard's destructor releases the lock while the panic unwinds.
                    struct Guard<'a>(&'a BatchSemaphore);
                    impl Drop for Guard<'_> {
                        fn drop(&mut self) {
                            self.0.release(1);
                        }
                    }
                    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let _guard = Guard(&s);
                        panic!("expected panic");
                    }));
                    assert!(panicked.is_err());
                    assert!(s.is_closed());
                    assert!(matches!(futures::poll!(a.as_mut()), Poll::Ready(Err(_))));
                    assert!(matches!(futures::poll!(b.as_mut()), Poll::Ready(Err(_))));
                });
            },
            None,
        );
    }

    /// Closing the semaphore forgets the woken waiters, so the releases of the permits that are
    /// still held, which a lock's guards make after the close, do not overtake them, and the
    /// stand-in grants none of them the permits of a closed semaphore.
    #[test_log::test]
    fn release_after_close_grants_no_woken_waiter() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(2);
                    s.acquire(2).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    s.release(1); // wakes a
                    s.try_acquire(1).unwrap();
                    s.release(1); // passes a once
                    s.try_acquire(1).unwrap();
                    s.close();
                    s.release(1); // would pass a a second time
                    s.release(1);
                    assert!(
                        matches!(futures::poll!(a.as_mut()), Poll::Ready(Err(_))),
                        "a closed semaphore granted a woken waiter"
                    );
                });
            },
            None,
        );
    }

    /// The same after a release while panicking has closed the semaphore, to model lock poisoning.
    #[test_log::test]
    fn release_after_poisoning_grants_no_woken_waiter() {
        check_dfs(
            || {
                future::block_on(async {
                    let s = sem(3);
                    s.acquire(3).await.unwrap();
                    let mut a = Box::pin(s.acquire(1));
                    pending!(a);
                    s.release(1); // wakes a
                    s.try_acquire(1).unwrap();

                    struct Guard<'a>(&'a BatchSemaphore);
                    impl Drop for Guard<'_> {
                        fn drop(&mut self) {
                            self.0.release(1);
                        }
                    }
                    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let _guard = Guard(&s);
                        panic!("expected panic");
                    }));
                    assert!(panicked.is_err());
                    assert!(s.is_closed());
                    s.release(1); // the other guards
                    s.release(1);
                    assert!(
                        matches!(futures::poll!(a.as_mut()), Poll::Ready(Err(_))),
                        "a poisoned semaphore granted a woken waiter"
                    );
                });
            },
            None,
        );
    }

    /// A woken waiter whose task has finished without dropping its `Acquire` will never try. A
    /// release forgets it, rather than count it as overtaken until the stand-in grants a finished
    /// task the permits, and the abandoned `Acquire` still works when a live task polls it (see
    /// `release_to_waiter_of_finished_task`).
    #[test_log::test]
    fn woken_waiter_of_a_finished_task_is_forgotten() {
        shuttle::lazy_static! {
            static ref SEM: BatchSemaphore = BatchSemaphore::new(1, Fairness::ParkingLot);
        }

        check_dfs(
            || {
                future::block_on(async {
                    SEM.acquire(1).await.unwrap();
                    let acquire = Arc::new(Mutex::new(None));
                    let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
                    let (released_tx, released_rx) = shuttle::sync::mpsc::channel::<()>();
                    let registrant = future::spawn({
                        let acquire = Arc::clone(&acquire);
                        async move {
                            let mut acq = Box::pin(SEM.acquire(1));
                            pending!(acq);
                            *acquire.lock().unwrap() = Some(acq);
                            parked_tx.send(()).unwrap();
                            // Stays alive until the release has woken the waiter.
                            released_rx.recv().unwrap();
                        }
                    });
                    parked_rx.recv().unwrap();
                    SEM.release(1); // wakes the registrant's waiter
                    released_tx.send(()).unwrap();
                    registrant.await.unwrap();

                    // More releases than the bound, each of which would overtake the woken waiter.
                    for _ in 0..4 {
                        SEM.try_acquire(1).unwrap();
                        SEM.release(1);
                        assert_eq!(SEM.available_permits(), 1);
                    }
                    let acq = acquire.lock().unwrap().take().unwrap();
                    acq.await.unwrap();
                    assert_eq!(SEM.available_permits(), 0);
                    SEM.release(1);
                });
            },
            None,
        );
    }

    /// A parked waiter of a task that has finished does not use up the wake of a release: the
    /// release passes over it, and wakes the live waiter behind it.
    #[test_log::test]
    fn release_passes_over_a_parked_waiter_of_a_finished_task() {
        shuttle::lazy_static! {
            static ref SEM: BatchSemaphore = BatchSemaphore::new(1, Fairness::ParkingLot);
        }
        check_dfs(
            || {
                future::block_on(async {
                    SEM.acquire(1).await.unwrap();
                    let acquire = Arc::new(Mutex::new(None));
                    let registrant = future::spawn({
                        let acquire = Arc::clone(&acquire);
                        async move {
                            let mut acq = Box::pin(SEM.acquire(1));
                            pending!(acq);
                            *acquire.lock().unwrap() = Some(acq);
                        }
                    });
                    registrant.await.unwrap();
                    let mut live = Box::pin(SEM.acquire(1));
                    pending!(live); // parks behind the abandoned waiter
                    SEM.release(1);
                    assert!(!SEM.has_waiters());
                    ready!(live);
                    SEM.release(1);
                    drop(acquire.lock().unwrap().take());
                });
            },
            None,
        );
    }

    /// A parked reserving acquire that a second task polls must wake that task, as for a plain
    /// acquire (see `queued_acquire_polled_by_second_task_is_woken`).
    #[test_log::test]
    fn parked_reserving_acquire_polled_by_second_task_is_woken() {
        use std::future::Future;
        use std::task::Context;

        shuttle::lazy_static! {
            static ref SEM: BatchSemaphore = BatchSemaphore::new(0, Fairness::ParkingLot);
            static ref PARK: BatchSemaphore = BatchSemaphore::new(0, Fairness::ParkingLot);
        }

        #[derive(Debug, Default)]
        struct Recorder(AtomicBool);
        impl futures::task::ArcWake for Recorder {
            fn wake_by_ref(arc_self: &Arc<Self>) {
                arc_self.0.store(true, Ordering::SeqCst);
            }
        }

        check_dfs(
            || {
                future::block_on(async move {
                    let acquire = Arc::new(Mutex::new(Some(Box::pin(SEM.acquire_reserving(1, 1)))));
                    let recorder_a = Arc::new(Recorder::default());
                    let recorder_b = Arc::new(Recorder::default());
                    let (polled_tx, polled_rx) = shuttle::sync::mpsc::channel();
                    let mut tasks = vec![];
                    for recorder in [recorder_a.clone(), recorder_b.clone()] {
                        let acquire = acquire.clone();
                        let polled_tx = polled_tx.clone();
                        tasks.push(future::spawn(async move {
                            let mut acq = acquire.lock().unwrap().take().unwrap();
                            {
                                let waker = futures::task::waker(recorder.clone());
                                let mut cx = Context::from_waker(&waker);
                                assert!(acq.as_mut().poll(&mut cx).is_pending());
                            }
                            *acquire.lock().unwrap() = Some(acq);
                            polled_tx.send(()).unwrap();
                            // Stays alive, so that the waiter is not stale.
                            PARK.acquire(1).await.unwrap();
                        }));
                        polled_rx.recv().unwrap();
                    }
                    SEM.release(1);
                    assert!(recorder_b.0.load(Ordering::SeqCst), "second poller was not woken");
                    assert!(!recorder_a.0.load(Ordering::SeqCst), "first poller was woken instead");
                    PARK.release(2);
                    for task in tasks {
                        task.await.unwrap();
                    }
                });
            },
            Some(500),
        );
    }

    /// A woken request whose `Acquire` a second task polls follows that task when it tries again,
    /// as a parked one does (see `queued_acquire_polled_by_second_task_outlives_its_first_task`),
    /// whether it parks again, also as a reserving request, or reserves the semaphore, also once
    /// the task that parked it has finished. The second task polls it once only, in `block_on`, so
    /// that no later poll of a parked request follows the task in its place. Each case is the
    /// permits, the `min_permits` of a reserving request, its permits, and the permits that the
    /// main task takes back after it woke the request.
    #[test_log::test]
    fn woken_acquire_polled_by_second_task_outlives_its_first_task() {
        use std::pin::Pin;
        type Stash = Arc<Mutex<Option<Pin<Box<Acquire<'static>>>>>>;
        shuttle::lazy_static! {
            static ref ONE: BatchSemaphore = BatchSemaphore::new(1, Fairness::ParkingLot);
            static ref MANY: BatchSemaphore = BatchSemaphore::new(N, Fairness::ParkingLot);
        }
        let cases: [(usize, Option<usize>, usize, usize); 3] = [
            (1, None, 1, 1),             // parks again
            (N, Some(RESERVE_AT), N, N), // parks again, as a reserving request
            (N, Some(RESERVE_AT), N, 1), // reserves
        ];
        for (permits, reserve_at, num_permits, taken_back) in cases {
            check_dfs(
                move || {
                    let s: &'static BatchSemaphore = if permits == 1 { &ONE } else { &MANY };
                    s.try_acquire(permits).unwrap();
                    let stash: Stash = Arc::new(Mutex::new(None));
                    let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
                    let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
                    let (polled_tx, polled_rx) = shuttle::sync::mpsc::channel::<()>();
                    let first = {
                        let stash = Arc::clone(&stash);
                        thread::spawn(move || {
                            future::block_on(async {
                                let mut acquire = Box::pin(match reserve_at {
                                    Some(min_permits) => s.acquire_reserving(min_permits, num_permits),
                                    None => s.acquire(num_permits),
                                });
                                pending!(acquire); // parks, for this task
                                *stash.lock().unwrap() = Some(acquire);
                            });
                            parked_tx.send(()).unwrap();
                            polled_rx.recv().unwrap();
                        })
                    };
                    let second = {
                        let stash = Arc::clone(&stash);
                        thread::spawn(move || {
                            go_rx.recv().unwrap();
                            let acquire = stash.lock().unwrap().take().unwrap();
                            // Lets the first task finish, and then tries, for this task, and waits.
                            polled_tx.send(()).unwrap();
                            future::block_on(acquire).unwrap();
                            s.release(num_permits);
                        })
                    };
                    parked_rx.recv().unwrap();
                    s.release(permits); // wakes the request
                    s.try_acquire(taken_back).unwrap(); // before it tries again
                    go_tx.send(()).unwrap();
                    first.join().unwrap(); // the task that parked the request has finished
                    s.release(taken_back);
                    second.join().unwrap();
                },
                None,
            );
        }
    }

    /// A release that drops a reservation whose task has finished leaves that task's waker with the
    /// abandoned `Acquire`. A live task that polls it then, and waits again, parked or holding the
    /// reservation again, must replace the waker, so that the next release wakes the live task.
    /// Each case is the permits that the main task takes before the live task polls.
    #[test_log::test]
    fn dropped_stale_reservation_polled_by_a_live_task_that_waits_again() {
        use std::pin::Pin;
        type Stash = Arc<Mutex<Option<Pin<Box<Acquire<'static>>>>>>;
        shuttle::lazy_static! {
            static ref SEM: BatchSemaphore = BatchSemaphore::new(3, Fairness::ParkingLot);
        }
        for taken in [0, 2] {
            check_dfs(
                move || {
                    SEM.try_acquire(2).unwrap();
                    let stash: Stash = Arc::new(Mutex::new(None));
                    let registrant = {
                        let stash = Arc::clone(&stash);
                        thread::spawn(move || {
                            future::block_on(async {
                                let mut acquire = Box::pin(SEM.acquire_reserving(1, 3));
                                pending!(acquire); // one permit is free: reserves
                                *stash.lock().unwrap() = Some(acquire);
                            });
                        })
                    };
                    registrant.join().unwrap(); // the holder's task has finished
                    SEM.release(1); // drops the stale reservation: two permits are free
                    if taken > 0 {
                        SEM.try_acquire(taken).unwrap(); // none is free
                    }
                    let (polled_tx, polled_rx) = shuttle::sync::mpsc::channel::<()>();
                    let live = {
                        let stash = Arc::clone(&stash);
                        thread::spawn(move || {
                            let acquire = stash.lock().unwrap().take().unwrap();
                            polled_tx.send(()).unwrap();
                            // Reserves again, or parks, for this task, and waits.
                            future::block_on(acquire).unwrap();
                            SEM.release(3);
                        })
                    };
                    polled_rx.recv().unwrap();
                    SEM.release(1 + taken); // all three are free
                    live.join().unwrap();
                },
                None,
            );
        }
    }

    /// The number of schedules of `program` on a semaphore of `fairness`, under exhaustive DFS,
    /// with `parking_lot_fairness_bound` as the bound.
    ///
    /// The liveness tests below check these numbers. At bound 0, every wake is a hand-off, so a
    /// program whose semaphore has one permit has as many schedules as on a strictly fair
    /// semaphore, which they compute. The numbers at the other bounds are measured, not derived:
    /// they pin how much the mode explores, and how many of its schedules the stand-in prunes, so
    /// a change to its wakes, grants, counts or scheduling points moves them. They grow with the
    /// bound, about twice for each step for a loop that takes and releases the lock, as the
    /// documentation of `Config::parking_lot_fairness_bound` says. A program that starves a
    /// request does not change them: it exceeds `max_steps`, and fails.
    fn dfs_schedules(fairness: Fairness, bound: usize, max_steps: usize, program: fn(Fairness)) -> usize {
        run_with_bound(DfsScheduler::new(None, false), bound, max_steps, move || {
            program(fairness)
        })
    }

    /// Tasks that take and release the lock in a loop while a waiter is parked. The stand-in
    /// serves the starving requests oldest first, rather than in the order in which a release
    /// woke them (see `stand_in_grants_the_oldest_request_first`), so that one is not overtaken
    /// forever by others that were woken before it.
    fn pingers_vs_parked_waiter(fairness: Fairness, pingers: usize) {
        let s = Arc::new(BatchSemaphore::new(1, fairness));
        s.try_acquire(1).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let waiter = spawn_parked_waiter(&s, &done);
        let pingers = (0..pingers)
            .map(|_| {
                let (s, done) = (Arc::clone(&s), Arc::clone(&done));
                thread::spawn(move || {
                    while !done.load(Ordering::SeqCst) {
                        s.acquire_blocking(1).unwrap();
                        s.release(1);
                    }
                })
            })
            .collect::<Vec<_>>();
        s.release(1);
        for pinger in pingers {
            pinger.join().unwrap();
        }
        future::block_on(waiter).unwrap();
    }

    fn one_pinger_vs_parked_waiter(fairness: Fairness) {
        pingers_vs_parked_waiter(fairness, 1)
    }

    fn two_pingers_vs_parked_waiter(fairness: Fairness) {
        pingers_vs_parked_waiter(fairness, 2)
    }

    /// Spawn a task whose acquire parks on `s` before this returns, and that sets `done` once it
    /// has had the permit. The `std` atomic adds no scheduling point.
    fn spawn_parked_waiter(s: &Arc<BatchSemaphore>, done: &Arc<AtomicBool>) -> future::JoinHandle<()> {
        spawn_parked_request(s, done, None, 1)
    }

    /// As `spawn_parked_waiter`, for a request of `num_permits` permits, which reserves the
    /// semaphore as soon as `reserve_at` are free if that is `Some`, as a writer of an `RwLock`
    /// does.
    fn spawn_parked_request(
        s: &Arc<BatchSemaphore>,
        done: &Arc<AtomicBool>,
        reserve_at: Option<usize>,
        num_permits: usize,
    ) -> future::JoinHandle<()> {
        let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
        let (s, done) = (Arc::clone(s), Arc::clone(done));
        let waiter = future::spawn(async move {
            let mut acquire = Box::pin(match reserve_at {
                Some(min_permits) => s.acquire_reserving(min_permits, num_permits),
                None => s.acquire(num_permits),
            });
            pending!(acquire);
            parked_tx.send(()).unwrap();
            acquire.await.unwrap();
            done.store(true, Ordering::SeqCst);
            s.release(num_permits);
        });
        parked_rx.recv().unwrap();
        waiter
    }

    /// The main task takes and releases the lock in a loop until a waiter that parked before the
    /// loop has had it. The waiter is the task that DFS runs last, so without the stand-in, the
    /// loop would never end.
    fn relock_loop_vs_parked_waiter(fairness: Fairness) {
        let s = Arc::new(BatchSemaphore::new(1, fairness));
        s.try_acquire(1).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let waiter = spawn_parked_waiter(&s, &done);
        while !done.load(Ordering::SeqCst) {
            s.release(1);
            s.acquire_blocking(1).unwrap();
        }
        s.release(1);
        future::block_on(waiter).unwrap();
    }

    /// As `relock_loop_vs_parked_waiter`, but the main task wakes the waiter first, and takes the
    /// lock back before it tries, and then releases with `release_fair`, as
    /// `MutexGuard::unlock_fair` does. A fair release that lets nobody in is a plain release, so
    /// the loop would starve the woken waiter without the stand-in at fair releases (see
    /// `fair_release_grants_a_starving_woken_waiter`).
    fn fair_relock_loop_vs_woken_waiter(fairness: Fairness) {
        let s = Arc::new(BatchSemaphore::new(1, fairness));
        s.try_acquire(1).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let waiter = spawn_parked_waiter(&s, &done);
        s.release(1); // wakes the waiter
        s.acquire_blocking(1).unwrap(); // takes the lock back before the waiter tries
        while !done.load(Ordering::SeqCst) {
            s.release_fair(1);
            s.acquire_blocking(1).unwrap();
        }
        s.release(1);
        future::block_on(waiter).unwrap();
    }

    /// As `relock_loop_vs_parked_waiter`, with `loopers` other tasks that take and release the
    /// lock in a loop, which start once the waiter has parked.
    fn loopers_vs_parked_waiter(fairness: Fairness, loopers: usize) {
        let s = Arc::new(BatchSemaphore::new(1, fairness));
        s.try_acquire(1).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let (starts, loopers): (Vec<_>, Vec<_>) = (0..loopers)
            .map(|_| {
                let (start_tx, start_rx) = shuttle::sync::mpsc::channel::<()>();
                let (s, done) = (Arc::clone(&s), Arc::clone(&done));
                let looper = thread::spawn(move || {
                    start_rx.recv().unwrap();
                    s.acquire_blocking(1).unwrap();
                    while !done.load(Ordering::SeqCst) {
                        s.release(1);
                        s.acquire_blocking(1).unwrap();
                    }
                    s.release(1);
                });
                (start_tx, looper)
            })
            .unzip();
        let waiter = spawn_parked_waiter(&s, &done);
        for start in starts {
            start.send(()).unwrap();
        }
        s.release(1);
        for looper in loopers {
            looper.join().unwrap();
        }
        future::block_on(waiter).unwrap();
    }

    fn one_looper_vs_parked_waiter(fairness: Fairness) {
        loopers_vs_parked_waiter(fairness, 1)
    }

    /// No schedule starves the waiter, and the number of schedules grows with the bound. At bound
    /// 0, every wake is a hand-off, and there are as many schedules as on a strictly fair
    /// semaphore.
    #[test_log::test]
    fn relock_loop_cannot_starve_a_parked_waiter() {
        let strictly_fair = dfs_schedules(Fairness::StrictlyFair, 0, 2_000, relock_loop_vs_parked_waiter);
        assert_eq!(strictly_fair, 12);
        for (bound, schedules) in [(0, strictly_fair), (1, 32), (2, 72), (3, 152)] {
            let executions = dfs_schedules(Fairness::ParkingLot, bound, 2_000, relock_loop_vs_parked_waiter);
            assert_eq!(executions, schedules, "bound {bound}");
        }
        for seed in 0..4 {
            for depth in [2, 3] {
                run_with_bound(PctScheduler::new_from_seed(seed, depth, 300), 2, 2_000, || {
                    relock_loop_vs_parked_waiter(Fairness::ParkingLot)
                });
            }
        }
        run_with_bound(RandomScheduler::new_from_seed(0, 300), 2, 2_000, || {
            relock_loop_vs_parked_waiter(Fairness::ParkingLot)
        });
    }

    /// The same with `release_fair`, after the waiter was woken. `Starver` runs the schedule that
    /// would starve it, in one execution, at each bound.
    #[test_log::test]
    fn fair_relock_loop_cannot_starve_a_woken_waiter() {
        for bound in [0, 1, 2, 3, 8] {
            run_with_bound(Starver::new(1), bound, 2_000, || {
                fair_relock_loop_vs_woken_waiter(Fairness::ParkingLot)
            });
        }
        let strictly_fair = dfs_schedules(Fairness::StrictlyFair, 0, 2_000, fair_relock_loop_vs_woken_waiter);
        assert_eq!(strictly_fair, 12);
        for (bound, schedules) in [(0, strictly_fair), (1, 32), (2, 52), (3, 72)] {
            let executions = dfs_schedules(Fairness::ParkingLot, bound, 2_000, fair_relock_loop_vs_woken_waiter);
            assert_eq!(executions, schedules, "bound {bound}");
        }
    }

    /// The same with a looping task of its own.
    #[test_log::test]
    fn looper_cannot_starve_a_parked_waiter() {
        let strictly_fair = dfs_schedules(Fairness::StrictlyFair, 0, 3_000, one_looper_vs_parked_waiter);
        assert_eq!(strictly_fair, 1_400);
        for (bound, schedules) in [(0, strictly_fair), (2, 5_540)] {
            let executions = dfs_schedules(Fairness::ParkingLot, bound, 3_000, one_looper_vs_parked_waiter);
            assert_eq!(executions, schedules, "bound {bound}");
        }
        for loopers in [2, 3] {
            for seed in 0..4 {
                for depth in [2, 3] {
                    run_with_bound(PctScheduler::new_from_seed(seed, depth, 300), 2, 3_000, move || {
                        loopers_vs_parked_waiter(Fairness::ParkingLot, loopers)
                    });
                }
            }
        }
    }

    /// The same with a task that takes and releases the lock in a loop.
    #[test_log::test]
    fn pinger_cannot_starve_a_parked_waiter() {
        let strictly_fair = dfs_schedules(Fairness::StrictlyFair, 0, 5_000, one_pinger_vs_parked_waiter);
        assert_eq!(strictly_fair, 40);
        for (bound, schedules) in [(0, strictly_fair), (2, 160)] {
            let executions = dfs_schedules(Fairness::ParkingLot, bound, 5_000, one_pinger_vs_parked_waiter);
            assert_eq!(executions, schedules, "bound {bound}");
        }
    }

    /// Two pingers. Their numbers of schedules pin the order of the stand-in, the oldest request
    /// first: in wake order, there are 114,424 at bound 1 (and `stand_in_grants_the_oldest_request_first`
    /// fails at once). No execution of this program takes more than 38 steps, so `max_steps` is
    /// small: a change that lets the pingers starve the waiter fails in about a second, rather than
    /// after minutes of DFS through executions that each take thousands of steps.
    #[test_log::test]
    fn starving_requests_are_served_oldest_first() {
        let strictly_fair = dfs_schedules(Fairness::StrictlyFair, 0, 200, two_pingers_vs_parked_waiter);
        assert_eq!(strictly_fair, 2_988);
        for (bound, schedules) in [(0, strictly_fair), (1, 38_000)] {
            let executions = dfs_schedules(Fairness::ParkingLot, bound, 200, two_pingers_vs_parked_waiter);
            assert_eq!(executions, schedules, "bound {bound}");
        }
    }

    /// As above, at the default bound, which takes about 6 seconds in release mode. This pin also
    /// catches a plain wake that takes the waiter's waker (459,804 schedules), and serving the
    /// starving requests in wake order (779,504).
    #[test_log::test]
    fn two_pingers_cannot_starve_a_parked_waiter() {
        let executions = dfs_schedules(Fairness::ParkingLot, 2, 200, two_pingers_vs_parked_waiter);
        assert_eq!(executions, 473_808);
    }

    /// As `two_pingers_vs_parked_waiter`, but the waiter is woken before the pingers ever release,
    /// and they release with `release_fair`, as `MutexGuard::unlock_fair` does. Each fair release
    /// hands the permit to the other pinger, which is parked, so no release leaves it free for the
    /// woken waiter, and the stand-in cannot grant it there. Once it starves, the stand-in has it
    /// try at a release instead, as its thread would have by then: it loses, and parks again, at
    /// the back, and the fair releases hand it the permit in its turn.
    fn fair_pingers_vs_woken_waiter(fairness: Fairness) {
        let s = Arc::new(BatchSemaphore::new(1, fairness));
        s.try_acquire(1).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let waiter = spawn_parked_waiter(&s, &done);
        let pingers = (0..2)
            .map(|_| {
                let (s, done) = (Arc::clone(&s), Arc::clone(&done));
                thread::spawn(move || {
                    while !done.load(Ordering::SeqCst) {
                        s.acquire_blocking(1).unwrap();
                        s.release_fair(1);
                    }
                })
            })
            .collect::<Vec<_>>();
        s.release(1); // wakes the waiter, at the front
        for pinger in pingers {
            pinger.join().unwrap();
        }
        future::block_on(waiter).unwrap();
    }

    /// As `fair_pingers_vs_woken_waiter`, for an `RwLock`: two writers unlock with
    /// `RwLockWriteGuard::unlock_fair`, and the waiter is a reader that a plain write unlock woke.
    fn fair_writers_vs_woken_reader(fairness: Fairness) {
        let s = Arc::new(BatchSemaphore::new(N, fairness));
        s.try_acquire(N).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let reader = spawn_parked_request(&s, &done, None, 1);
        let writers = (0..2)
            .map(|_| {
                let (s, done) = (Arc::clone(&s), Arc::clone(&done));
                thread::spawn(move || {
                    while !done.load(Ordering::SeqCst) {
                        write(&s);
                        s.release_fair(N);
                    }
                })
            })
            .collect::<Vec<_>>();
        s.release(N); // `unlock_exclusive`: wakes the reader, at the front
        for writer in writers {
            writer.join().unwrap();
        }
        future::block_on(reader).unwrap();
    }

    /// No schedule of two tasks that hand the lock to each other with `release_fair` starves a
    /// woken waiter. `Starver` runs the schedule that would, in one execution, so this fails at
    /// once if they can starve it, at any bound. The numbers of schedules under DFS are pinned
    /// (see `dfs_schedules`), with a small `max_steps`, as for `starving_requests_are_served_oldest_first`:
    /// no execution takes more than 34 steps. At bound 0, every wake is a hand-off, so no woken
    /// waiter is left to starve.
    #[test_log::test]
    fn fair_pingers_cannot_starve_a_woken_waiter() {
        for bound in [0, 1, 2, 3, 8] {
            run_with_bound(Starver::new(1), bound, 2_000, || {
                fair_pingers_vs_woken_waiter(Fairness::ParkingLot)
            });
        }
        let strictly_fair = dfs_schedules(Fairness::StrictlyFair, 0, 200, fair_pingers_vs_woken_waiter);
        assert_eq!(strictly_fair, 2_988);
        for (bound, schedules) in [(0, strictly_fair), (1, 62_260), (2, 260_200)] {
            let executions = dfs_schedules(Fairness::ParkingLot, bound, 200, fair_pingers_vs_woken_waiter);
            assert_eq!(executions, schedules, "bound {bound}");
        }
    }

    /// The same for an `RwLock`, whose executions take up to 35 steps. A strictly fair semaphore
    /// has no reservations, so it cannot run this program.
    #[test_log::test]
    fn fair_writers_cannot_starve_a_woken_reader() {
        for bound in [0, 1, 2, 3, 8] {
            run_with_bound(Starver::new(1), bound, 2_000, || {
                fair_writers_vs_woken_reader(Fairness::ParkingLot)
            });
        }
        for (bound, schedules) in [(0, 5_132), (1, 101_216), (2, 442_528)] {
            let executions = dfs_schedules(Fairness::ParkingLot, bound, 200, fair_writers_vs_woken_reader);
            assert_eq!(executions, schedules, "bound {bound}");
        }
    }

    /// As `relock_loop_vs_parked_waiter`, with a writer of an `RwLock` (a reserving acquire) as the
    /// waiter, which loses the race each time the main task takes the lock first.
    fn relock_loop_vs_parked_writer(fairness: Fairness) {
        let s = Arc::new(BatchSemaphore::new(N, fairness));
        s.try_acquire(N).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let writer = spawn_parked_request(&s, &done, Some(RESERVE_AT), N);
        while !done.load(Ordering::SeqCst) {
            s.release(N);
            s.acquire_blocking(N).unwrap();
        }
        s.release(N);
        future::block_on(writer).unwrap();
    }

    /// The tries that a writer loses overtake it too, so a relock loop cannot starve it either,
    /// with as many schedules as for a waiter of a `Mutex` at each bound (see
    /// `relock_loop_cannot_starve_a_parked_waiter`): the reservation adds none.
    #[test_log::test]
    fn relock_loop_cannot_starve_a_parked_writer() {
        for bound in 0..4 {
            let mutex = dfs_schedules(Fairness::ParkingLot, bound, 2_000, relock_loop_vs_parked_waiter);
            let executions = dfs_schedules(Fairness::ParkingLot, bound, 2_000, relock_loop_vs_parked_writer);
            assert_eq!(executions, mutex, "bound {bound}");
        }
    }

    /// A writer that a release woke never runs, while a reader takes and releases the lock in a
    /// loop with `release_without_waking`, as the read guards of an `RwLock` do. Those shared
    /// releases overtake the writer, so the stand-in grants it: here it is handed the reservation,
    /// and the reader's next release wakes it.
    fn read_spinner_vs_woken_writer(fairness: Fairness) {
        let s = Arc::new(BatchSemaphore::new(N, fairness));
        s.try_acquire(N).unwrap(); // a writer holds the lock
        let done = Arc::new(AtomicBool::new(false));
        let writer = spawn_parked_request(&s, &done, Some(RESERVE_AT), N);
        let spinner = {
            let (s, done) = (Arc::clone(&s), Arc::clone(&done));
            thread::spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    s.acquire_blocking(1).unwrap();
                    s.release_without_waking(1);
                }
            })
        };
        s.release(N); // wakes the writer
        spinner.join().unwrap();
        future::block_on(writer).unwrap();
    }

    /// The number of schedules is pinned (see `dfs_schedules`). A strictly fair semaphore has no
    /// reservations, so it cannot run this program.
    #[test_log::test]
    fn read_spinner_cannot_starve_a_woken_writer() {
        let executions = dfs_schedules(Fairness::ParkingLot, 2, 3_000, read_spinner_vs_woken_writer);
        assert_eq!(executions, 120);
        for seed in 0..4 {
            for depth in [2, 3] {
                run_with_bound(PctScheduler::new_from_seed(seed, depth, 300), 2, 3_000, || {
                    read_spinner_vs_woken_writer(Fairness::ParkingLot)
                });
            }
        }
    }

    /// Whether a thread is parked on the lock, as `shuttle-parking_lot` reads `PARKED_BIT`: in one
    /// read of the state, which is a scheduling point.
    fn parked(s: &BatchSemaphore) -> bool {
        s.load_permits().is_some() && s.has_waiters()
    }

    /// `RwLock::write` of `shuttle-parking_lot`: a writer reserves the lock as soon as no other
    /// writer or upgradable reader holds it.
    fn write(s: &BatchSemaphore) {
        future::block_on(s.acquire_reserving(RESERVE_AT, N)).unwrap();
    }

    /// The main task holds the lock while a waiter is parked, unlocks, which wakes the waiter, and
    /// locks again before the waiter tries. Then it calls `MutexGuard::bump`, which hands the lock
    /// off to a parked thread, until the waiter has had the lock. The waiter is the task that DFS
    /// runs last, so it never tries, and `bump` sees no parked thread. Its reads of the state
    /// overtake the woken waiter, though, so it tries at one, and loses: it parks again, which the
    /// `bump` sees. On `parking_lot`, the waiter tries soon after its wake, and parks again too.
    fn mutex_bump_loop_vs_woken_waiter(fairness: Fairness) {
        let s = Arc::new(BatchSemaphore::new(1, fairness));
        s.try_acquire(1).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let waiter = spawn_parked_waiter(&s, &done);
        s.release(1); // wakes the waiter
        s.acquire_blocking(1).unwrap(); // locks again before the waiter tries
        while !done.load(Ordering::SeqCst) {
            // `bump`
            if parked(&s) {
                s.release_fair(1);
                s.acquire_blocking(1).unwrap();
            }
        }
        s.release(1);
        future::block_on(waiter).unwrap();
    }

    /// As above, with `RwLockWriteGuard::bump` and a reader parked behind the writer.
    fn write_bump_loop_vs_woken_reader(fairness: Fairness) {
        let s = Arc::new(BatchSemaphore::new(N, fairness));
        s.try_acquire(N).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let reader = spawn_parked_request(&s, &done, None, 1);
        s.release(N); // `unlock_exclusive`: wakes the reader
        write(&s); // locks again before the reader tries
        while !done.load(Ordering::SeqCst) {
            // `bump_exclusive`
            if parked(&s) {
                s.release_fair(N);
                write(&s);
            }
        }
        s.release(N);
        future::block_on(reader).unwrap();
    }

    /// As above, with `RwLockUpgradableReadGuard::bump` and a writer parked behind the
    /// upgradable reader.
    fn upgradable_bump_loop_vs_woken_writer(fairness: Fairness) {
        let s = Arc::new(BatchSemaphore::new(N, fairness));
        s.try_acquire(UPG).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let writer = spawn_parked_request(&s, &done, Some(RESERVE_AT), N);
        s.release(UPG); // `unlock_upgradable`: wakes the writer
        s.acquire_blocking(UPG).unwrap(); // locks again before the writer tries
        while !done.load(Ordering::SeqCst) {
            // `bump_upgradable`
            if parked(&s) {
                s.release_fair(UPG);
                s.acquire_blocking(UPG).unwrap();
            }
        }
        s.release(UPG);
        future::block_on(writer).unwrap();
    }

    /// As above, with `RwLockReadGuard::bump`, which unlocks and locks again only while a writer
    /// holds `WRITER_BIT` (the reservation, which leaves no permit free), after a downgrade woke a
    /// writer. The reads of the bump overtake the writer, so it tries at one, and reserves the
    /// lock, which the bump sees.
    fn read_bump_loop_vs_woken_writer(fairness: Fairness) {
        let s = Arc::new(BatchSemaphore::new(N, fairness));
        s.try_acquire(N).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let writer = spawn_parked_request(&s, &done, Some(RESERVE_AT), N);
        s.release(N - 1); // `downgrade`: wakes the writer
        while !done.load(Ordering::SeqCst) {
            // `bump_shared`
            if s.load_permits() == Some(0) {
                s.release_without_waking(1);
                s.acquire_blocking(1).unwrap();
            }
        }
        s.release_without_waking(1);
        future::block_on(writer).unwrap();
    }

    /// The main task unlocks, which wakes the waiter, and then waits for the waiter to take the
    /// lock by reading its state (`is_locked`), without ever releasing.
    fn is_locked_loop_vs_woken_waiter(fairness: Fairness) {
        let s = Arc::new(BatchSemaphore::new(1, fairness));
        s.try_acquire(1).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let waiter = spawn_parked_waiter(&s, &done);
        s.release(1); // wakes the waiter
        while !done.load(Ordering::SeqCst) && s.load_permits() == Some(1) {}
        future::block_on(waiter).unwrap();
    }

    /// No schedule of a loop of `bump`s, or of reads of the lock's state, starves a woken waiter,
    /// under exhaustive DFS, with the number of schedules checked for bounds 0 to 2, and under PCT,
    /// which runs the oldest task first in its first execution. At bound 0, the programs on one
    /// permit have as many schedules as on a strictly fair semaphore, and the `bump`s of a write
    /// lock and of an upgradable read lock have as many as that of a `Mutex` at every bound. The
    /// others are pinned (see `dfs_schedules`).
    #[test_log::test]
    fn read_loops_cannot_starve_a_woken_waiter() {
        let check = |name: &str, program: fn(Fairness), schedules: [usize; 3]| {
            for (bound, schedules) in schedules.into_iter().enumerate() {
                let executions = dfs_schedules(Fairness::ParkingLot, bound, 2_000, program);
                assert_eq!(executions, schedules, "{name}, bound {bound}");
            }
            for seed in 0..4 {
                for depth in [2, 3] {
                    run_with_bound(PctScheduler::new_from_seed(seed, depth, 300), 2, 2_000, move || {
                        program(Fairness::ParkingLot)
                    });
                }
            }
        };

        let strictly_fair = dfs_schedules(Fairness::StrictlyFair, 0, 2_000, mutex_bump_loop_vs_woken_waiter);
        assert_eq!(strictly_fair, 12);
        let mutex_bump = [strictly_fair, 44, 56];
        check("Mutex bump", mutex_bump_loop_vs_woken_waiter, mutex_bump);
        check("write bump", write_bump_loop_vs_woken_reader, mutex_bump);
        check("upgradable bump", upgradable_bump_loop_vs_woken_writer, mutex_bump);
        check("read bump", read_bump_loop_vs_woken_writer, [36, 36, 48]);
        let strictly_fair = dfs_schedules(Fairness::StrictlyFair, 0, 2_000, is_locked_loop_vs_woken_waiter);
        assert_eq!(strictly_fair, 12);
        check("is_locked", is_locked_loop_vs_woken_waiter, [strictly_fair, 12, 20]);
    }

    /// A scheduler that records the schedule of the execution that it runs, so that a test can
    /// replay a failure without persisting it: the panic hook that persists the schedule of a
    /// failure is installed once per process, with the `Config` of the first `Runner`.
    struct Recording<S> {
        inner: S,
        schedule: Arc<Mutex<Option<Schedule>>>,
    }

    impl<S: Scheduler> Scheduler for Recording<S> {
        fn new_execution(&mut self) -> Option<Schedule> {
            let schedule = self.inner.new_execution()?;
            *self.schedule.lock().unwrap() = Some(schedule.clone());
            Some(schedule)
        }

        fn next_task(
            &mut self,
            runnable_tasks: &[&Task],
            current_task: Option<TaskId>,
            is_yielding: bool,
        ) -> Option<TaskId> {
            let task = self.inner.next_task(runnable_tasks, current_task, is_yielding)?;
            self.schedule.lock().unwrap().as_mut().unwrap().push_task(task);
            Some(task)
        }

        fn next_u64(&mut self) -> u64 {
            self.schedule.lock().unwrap().as_mut().unwrap().push_random();
            self.inner.next_u64()
        }
    }

    /// A scheduler that runs one execution, in which it runs the task `starved` only when no other
    /// task can run, and otherwise moves on from the current task to the next one that can, in the
    /// order of their ids. So two other tasks that take turns at the semaphore never let `starved`
    /// run: if they can starve it, they do, and the execution exceeds `max_steps`. DFS explores
    /// this schedule too, but only after a great many others.
    struct Starver {
        starved: TaskId,
        done: bool,
    }

    impl Starver {
        fn new(starved: usize) -> Self {
            Self {
                starved: TaskId::from(starved),
                done: false,
            }
        }
    }

    impl Scheduler for Starver {
        fn new_execution(&mut self) -> Option<Schedule> {
            (!std::mem::replace(&mut self.done, true)).then(|| Schedule::new(0))
        }

        fn next_task(
            &mut self,
            runnable_tasks: &[&Task],
            current_task: Option<TaskId>,
            _is_yielding: bool,
        ) -> Option<TaskId> {
            let others = || {
                runnable_tasks
                    .iter()
                    .map(|task| task.id())
                    .filter(|&id| id != self.starved)
            };
            let after_current = |id: TaskId| current_task.is_some_and(|current| usize::from(id) > usize::from(current));
            others()
                .find(|&id| after_current(id))
                .or_else(|| others().next())
                .or(Some(self.starved))
        }

        fn next_u64(&mut self) -> u64 {
            0
        }
    }

    /// The message of a panic.
    fn message(payload: &(dyn std::any::Any + Send)) -> String {
        match payload.downcast_ref::<&str>() {
            Some(message) => message.to_string(),
            None => payload.downcast_ref::<String>().cloned().unwrap_or_default(),
        }
    }

    /// Find a schedule in which `program` panics with "expected panic" under DFS, after it stored
    /// the clock of the failure in `failure_clock`, and return the message of the panic of a
    /// replay of that schedule up to that clock (see `ReplayScheduler::set_target_clock`).
    fn replay_up_to_the_failure_clock(program: fn(), failure_clock: &'static std::sync::Mutex<Vec<u32>>) -> String {
        let recorded = Arc::new(Mutex::new(None));
        let found = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut config = Config::new();
            config.failure_persistence = FailurePersistence::None;
            let scheduler = Recording {
                inner: DfsScheduler::new(None, false),
                schedule: Arc::clone(&recorded),
            };
            Runner::new(scheduler, config).run(program);
        }))
        .expect_err("no schedule fails");
        assert_eq!(message(&*found), "expected panic");
        let schedule = recorded.lock().unwrap().take().expect("no schedule was recorded");
        let clock = failure_clock.lock().unwrap().clone();

        let replayed = std::panic::catch_unwind(|| {
            let mut scheduler = ReplayScheduler::new_from_schedule(schedule);
            scheduler.set_target_clock(&clock[..]);
            let mut config = Config::new();
            config.failure_persistence = FailurePersistence::None;
            Runner::new(scheduler, config).run(program);
        })
        .expect_err("the replay did not fail");
        message(&*replayed)
    }

    /// A replay up to the clock of a failure (see `ReplayScheduler::set_target_clock`) schedules
    /// only the steps that the failure is causally after, so it reproduces a failure that depends on
    /// a grant of the eventual-fairness stand-in only if the releases are after the events that
    /// the stand-in counted. Here the main task fails if, right after it released its permit, the
    /// waiter holds a permit that it has not taken itself: a release granted it, after a pinger
    /// woke the waiter and overtook it. The main task takes no permit that the pinger released,
    /// and the pinger ends with a release, so the main task is after those releases only through
    /// the woken waiters' clock (`last_woken`). The flags are `std` atomics, which add no
    /// scheduling point and no clock edge.
    #[test_log::test]
    fn target_clock_replay_reproduces_a_failure_after_a_stand_in_grant() {
        static FAILURE_CLOCK: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

        fn waiter_is_granted_a_permit() {
            let s = Arc::new(sem(2));
            s.try_acquire(1).unwrap();
            let waiter_took = Arc::new(AtomicBool::new(false));
            let pinger_holds = Arc::new(AtomicBool::new(false));
            let waiter = {
                let (s, waiter_took) = (Arc::clone(&s), Arc::clone(&waiter_took));
                thread::spawn(move || {
                    s.acquire_blocking(1).unwrap();
                    waiter_took.store(true, Ordering::SeqCst);
                    s.release(1);
                })
            };
            let pinger = {
                let (s, pinger_holds) = (Arc::clone(&s), Arc::clone(&pinger_holds));
                thread::spawn(move || {
                    for _ in 0..2 {
                        if s.try_acquire(1).is_ok() {
                            pinger_holds.store(true, Ordering::SeqCst);
                            s.release(1);
                            pinger_holds.store(false, Ordering::SeqCst);
                        }
                    }
                })
            };
            s.release(1);
            // No scheduling point since the release: the permit that is not free is the waiter's,
            // granted to it by a release, if neither the pinger holds it nor the waiter took it.
            let granted = s.available_permits() == 1
                && !pinger_holds.load(Ordering::SeqCst)
                && !waiter_took.load(Ordering::SeqCst);
            if granted {
                *FAILURE_CLOCK.lock().unwrap() = current::clock().to_vec();
                panic!("expected panic");
            }
            waiter.join().unwrap();
            pinger.join().unwrap();
        }

        assert_eq!(
            replay_up_to_the_failure_clock(waiter_is_granted_a_permit, &FAILURE_CLOCK),
            "expected panic"
        );
    }

    /// As above, but the waiter does not try again after the pinger's release woke it, as it waits
    /// for `go` first, and it and the pinger have events of their own, between which the replay
    /// can stop them: a replay schedules a step of a task only if the task's clock before the step
    /// is below the target. So the failure depends on the waiter's park, which the pinger's
    /// release that woke it read (`last_queue_change`), and on the pinger's second release, which
    /// passed the waiter (`last_woken`).
    #[test_log::test]
    fn target_clock_replay_of_a_grant_to_a_waiter_that_never_tried_again() {
        static FAILURE_CLOCK: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

        fn waiter_is_granted_a_permit() {
            let s = Arc::new(sem(2));
            s.try_acquire(1).unwrap();
            let waiter_took = Arc::new(AtomicBool::new(false));
            let pinger_holds = Arc::new(AtomicBool::new(false));
            let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
            let waiter = {
                let (s, waiter_took) = (Arc::clone(&s), Arc::clone(&waiter_took));
                future::spawn(async move {
                    BatchSemaphore::new(0, Fairness::Unfair).release(1);
                    let mut acquire = Box::pin(s.acquire(1));
                    if futures::poll!(acquire.as_mut()).is_ready() {
                        waiter_took.store(true, Ordering::SeqCst);
                        s.release(1);
                        return;
                    }
                    go_rx.recv().unwrap();
                    acquire.await.unwrap();
                    s.release(1);
                })
            };
            let pinger = {
                let (s, pinger_holds) = (Arc::clone(&s), Arc::clone(&pinger_holds));
                thread::spawn(move || {
                    for _ in 0..2 {
                        if s.try_acquire(1).is_ok() {
                            pinger_holds.store(true, Ordering::SeqCst);
                            s.release(1);
                            pinger_holds.store(false, Ordering::SeqCst);
                        }
                        BatchSemaphore::new(0, Fairness::Unfair).release(1);
                    }
                })
            };
            s.release(1);
            let granted = s.available_permits() == 1
                && !pinger_holds.load(Ordering::SeqCst)
                && !waiter_took.load(Ordering::SeqCst);
            if granted {
                *FAILURE_CLOCK.lock().unwrap() = current::clock().to_vec();
                panic!("expected panic");
            }
            let _ = go_tx.send(());
            pinger.join().unwrap();
            future::block_on(waiter).unwrap();
        }

        assert_eq!(
            replay_up_to_the_failure_clock(waiter_is_granted_a_permit, &FAILURE_CLOCK),
            "expected panic"
        );
    }

    /// A replay up to the clock of a failure of a waiter that a release let in on the permit of an
    /// older release reproduces it (see
    /// `waiter_let_in_on_the_permit_of_an_older_release_is_after_the_release`). The main task
    /// has an event of its own between the two releases, so a replay that left out the release
    /// that let the waiter in would stop the main task before it, and never let the waiter in.
    #[test_log::test]
    fn target_clock_replay_of_a_waiter_let_in_on_the_permit_of_an_older_release() {
        static FAILURE_CLOCK: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

        fn waiter_let_in_on_an_older_permit(fair: bool) {
            let s = Arc::new(sem(2));
            s.try_acquire(2).unwrap();
            let waiter = {
                let s = Arc::clone(&s);
                thread::spawn(move || {
                    future::block_on(async {
                        let mut acquire = Box::pin(s.acquire(1));
                        if futures::poll!(acquire.as_mut()).is_ready() {
                            s.release(1);
                            return;
                        }
                        acquire.await.unwrap();
                        *FAILURE_CLOCK.lock().unwrap() = current::clock().to_vec();
                        panic!("expected panic");
                    })
                })
            };
            s.release_without_waking(1); // frees a permit, and wakes nobody
            BatchSemaphore::new(0, Fairness::Unfair).release(1);
            if fair {
                s.release_fair(1); // hands a parked waiter the older permit
            } else {
                s.release(1); // wakes a parked waiter, which takes the older permit
            }
            waiter.join().unwrap();
        }

        fn fair() {
            waiter_let_in_on_an_older_permit(true)
        }
        fn plain() {
            waiter_let_in_on_an_older_permit(false)
        }
        for program in [fair as fn(), plain] {
            assert_eq!(
                replay_up_to_the_failure_clock(program, &FAILURE_CLOCK),
                "expected panic"
            );
        }
    }

    /// The same for the stranded reader of `parking_lot`, with the calls of `shuttle-parking_lot`'s
    /// `RwLock`: the main task's write unlock wakes the writer `w` only, the main task takes an
    /// upgradable read first, `w` loses and parks again behind the reader `r`, the main task takes
    /// and releases a read, and its upgradable unlock, after an event of its own, lets `r` in on
    /// the permit of the read. `r` fails if it gets in after the upgradable unlock and before `w`
    /// has written.
    #[test_log::test]
    fn target_clock_replay_of_a_stranded_reader_let_in_by_an_upgradable_unlock() {
        static FAILURE_CLOCK: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
        static UPGRADABLE_UNLOCKED: AtomicBool = AtomicBool::new(false);
        static WRITTEN: AtomicBool = AtomicBool::new(false);

        fn stranded_reader() {
            UPGRADABLE_UNLOCKED.store(false, Ordering::SeqCst);
            WRITTEN.store(false, Ordering::SeqCst);
            let s = Arc::new(sem(N));
            s.try_acquire(N).unwrap(); // a write lock
            let w = {
                let s = Arc::clone(&s);
                thread::spawn(move || {
                    future::block_on(s.acquire_reserving(RESERVE_AT, N)).unwrap(); // `write`
                    WRITTEN.store(true, Ordering::SeqCst);
                    s.release(N); // `unlock_exclusive`
                })
            };
            let r = {
                let s = Arc::clone(&s);
                thread::spawn(move || {
                    future::block_on(async {
                        let mut read = Box::pin(s.acquire(1)); // `read`
                        if futures::poll!(read.as_mut()).is_ready() {
                            s.release_without_waking(1);
                            return;
                        }
                        read.await.unwrap();
                        if UPGRADABLE_UNLOCKED.load(Ordering::SeqCst) && !WRITTEN.load(Ordering::SeqCst) {
                            *FAILURE_CLOCK.lock().unwrap() = current::clock().to_vec();
                            panic!("expected panic");
                        }
                        s.release_without_waking(1); // `unlock_shared`
                    })
                })
            };
            s.release(N); // `unlock_exclusive`: wakes the waiter at the front
            s.acquire_blocking(UPG).unwrap(); // `upgradable_read`, ahead of the woken waiters
            s.acquire_blocking(1).unwrap(); // `read`
            s.release_without_waking(1); // `unlock_shared`
            BatchSemaphore::new(0, Fairness::Unfair).release(1);
            s.release(UPG); // `unlock_upgradable`
            UPGRADABLE_UNLOCKED.store(true, Ordering::SeqCst);
            w.join().unwrap();
            r.join().unwrap();
        }

        assert_eq!(
            replay_up_to_the_failure_clock(stranded_reader, &FAILURE_CLOCK),
            "expected panic"
        );
    }

    /// A release that takes waiters out of the queue changes it, so a later release, which reads
    /// the queue, is after it (see `ParkingLotState::last_queue_change`), also a fair release, which
    /// grants them and so changes no woken waiter. A replay up to the clock of a failure of a
    /// waiter that the later release let in then has the earlier one, and lets the same waiter in.
    /// Here `a` parks, and `b` behind it, and `h` and the main task each release a permit, after
    /// an event of its own, `h` with `release_fair` or `release`. `b` fails if `h` released
    /// first: then `h`'s release let `a` in, and the main task's let `b` in.
    #[test_log::test]
    fn target_clock_replay_of_a_waiter_let_in_after_a_release_that_took_another_out() {
        static FAILURE_CLOCK: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
        static MAIN_RELEASED: AtomicBool = AtomicBool::new(false);
        static H_RELEASED_FIRST: AtomicBool = AtomicBool::new(false);

        fn waiter_let_in_second(fair: bool) {
            MAIN_RELEASED.store(false, Ordering::SeqCst);
            H_RELEASED_FIRST.store(false, Ordering::SeqCst);
            let s = Arc::new(sem(2));
            s.try_acquire(1).unwrap();
            let (held_tx, held_rx) = shuttle::sync::mpsc::channel::<()>();
            let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
            let h = {
                let s = Arc::clone(&s);
                thread::spawn(move || {
                    s.try_acquire(1).unwrap();
                    held_tx.send(()).unwrap();
                    go_rx.recv().unwrap();
                    BatchSemaphore::new(0, Fairness::Unfair).release(1);
                    if fair {
                        s.release_fair(1);
                    } else {
                        s.release(1);
                    }
                    H_RELEASED_FIRST.store(!MAIN_RELEASED.load(Ordering::SeqCst), Ordering::SeqCst);
                })
            };
            held_rx.recv().unwrap();
            let (a_parked_tx, a_parked_rx) = shuttle::sync::mpsc::channel::<()>();
            let (checked_tx, checked_rx) = shuttle::sync::mpsc::channel::<()>();
            let a = {
                let s = Arc::clone(&s);
                future::spawn(async move {
                    let mut acquire = Box::pin(s.acquire(1));
                    pending!(acquire);
                    a_parked_tx.send(()).unwrap();
                    acquire.await.unwrap();
                    let _ = checked_rx.recv(); // keeps its permit until `b` has checked
                    s.release(1);
                })
            };
            a_parked_rx.recv().unwrap();
            let (b_parked_tx, b_parked_rx) = shuttle::sync::mpsc::channel::<()>();
            let b = {
                let s = Arc::clone(&s);
                future::spawn(async move {
                    let mut acquire = Box::pin(s.acquire(1));
                    pending!(acquire);
                    b_parked_tx.send(()).unwrap();
                    acquire.await.unwrap();
                    if H_RELEASED_FIRST.load(Ordering::SeqCst) {
                        *FAILURE_CLOCK.lock().unwrap() = current::clock().to_vec();
                        panic!("expected panic");
                    }
                    let _ = checked_tx.send(());
                    s.release(1);
                })
            };
            b_parked_rx.recv().unwrap();
            go_tx.send(()).unwrap();
            BatchSemaphore::new(0, Fairness::Unfair).release(1);
            s.release(1);
            MAIN_RELEASED.store(true, Ordering::SeqCst);
            h.join().unwrap();
            future::block_on(a).unwrap();
            future::block_on(b).unwrap();
        }

        fn fair() {
            waiter_let_in_second(true)
        }
        fn plain() {
            waiter_let_in_second(false)
        }
        for program in [fair as fn(), plain] {
            assert_eq!(
                replay_up_to_the_failure_clock(program, &FAILURE_CLOCK),
                "expected panic"
            );
        }
    }

    /// The same for a parked `Acquire` that is dropped, which takes its waiter out of the queue,
    /// and wakes nobody (see `dropped_parked_acquire_wakes_nobody`): a later release is after the
    /// drop. Here `a` parks, and `b` behind it, and `a`'s task drops `a` after an event of its
    /// own. `b` fails if the drop came before the main task's release, which then lets `b` in.
    #[test_log::test]
    fn target_clock_replay_of_a_waiter_let_in_after_a_dropped_parked_acquire() {
        static FAILURE_CLOCK: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
        static RELEASED: AtomicBool = AtomicBool::new(false);
        static DROPPED_FIRST: AtomicBool = AtomicBool::new(false);

        fn waiter_behind_a_dropped_acquire() {
            RELEASED.store(false, Ordering::SeqCst);
            DROPPED_FIRST.store(false, Ordering::SeqCst);
            let s = Arc::new(sem(1));
            s.try_acquire(1).unwrap();
            let (a_parked_tx, a_parked_rx) = shuttle::sync::mpsc::channel::<()>();
            let a = {
                let s = Arc::clone(&s);
                future::spawn(async move {
                    let mut acquire = Box::pin(s.acquire(1));
                    pending!(acquire);
                    a_parked_tx.send(()).unwrap();
                    BatchSemaphore::new(0, Fairness::Unfair).release(1);
                    future::yield_now().await;
                    DROPPED_FIRST.store(!RELEASED.load(Ordering::SeqCst), Ordering::SeqCst);
                    drop(acquire); // still parked, unless the release has woken it
                })
            };
            a_parked_rx.recv().unwrap();
            let (b_parked_tx, b_parked_rx) = shuttle::sync::mpsc::channel::<()>();
            let b = {
                let s = Arc::clone(&s);
                future::spawn(async move {
                    let mut acquire = Box::pin(s.acquire(1));
                    pending!(acquire);
                    b_parked_tx.send(()).unwrap();
                    acquire.await.unwrap();
                    if DROPPED_FIRST.load(Ordering::SeqCst) {
                        *FAILURE_CLOCK.lock().unwrap() = current::clock().to_vec();
                        panic!("expected panic");
                    }
                    s.release(1);
                })
            };
            b_parked_rx.recv().unwrap();
            s.release(1);
            RELEASED.store(true, Ordering::SeqCst);
            future::block_on(a).unwrap();
            future::block_on(b).unwrap();
        }

        assert_eq!(
            replay_up_to_the_failure_clock(waiter_behind_a_dropped_acquire, &FAILURE_CLOCK),
            "expected panic"
        );
    }

    /// A failed `try_acquire_without_overtaking` (a `try_write`) is causally after the release that
    /// freed the permit its holder took, as `load_permits` is, and not only after the holder's
    /// acquire: a replay up to its clock must not leave out task A's release, or task B, which
    /// takes A's permit, parks instead, and the replay ends before the failure.
    #[test_log::test]
    fn failed_try_acquire_without_overtaking_is_after_the_release_the_holder_took() {
        static FAILURE_CLOCK: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
        static B_HOLDS: AtomicBool = AtomicBool::new(false);

        fn holder_took_a_released_permit() {
            B_HOLDS.store(false, Ordering::SeqCst);
            let s = Arc::new(sem(1));
            let a = {
                let s = Arc::clone(&s);
                thread::spawn(move || {
                    s.acquire_blocking(1).unwrap();
                    s.release(1);
                })
            };
            let b = {
                let s = Arc::clone(&s);
                thread::spawn(move || {
                    s.acquire_blocking(1).unwrap();
                    B_HOLDS.store(true, Ordering::SeqCst);
                    thread::yield_now();
                    B_HOLDS.store(false, Ordering::SeqCst);
                    s.release(1);
                })
            };
            match s.try_acquire_without_overtaking(1) {
                Ok(()) => s.release(1),
                Err(_) if B_HOLDS.load(Ordering::SeqCst) => {
                    *FAILURE_CLOCK.lock().unwrap() = current::clock().to_vec();
                    panic!("expected panic");
                }
                Err(_) => {}
            }
            a.join().unwrap();
            b.join().unwrap();
        }

        assert_eq!(
            replay_up_to_the_failure_clock(holder_took_a_released_permit, &FAILURE_CLOCK),
            "expected panic"
        );
    }

    /// As `failed_try_acquire_without_overtaking_is_after_the_release_the_holder_took`, for
    /// `try_acquire` (a `Mutex::try_lock`).
    #[test_log::test]
    fn failed_try_acquire_is_after_the_release_the_holder_took() {
        static FAILURE_CLOCK: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
        static B_HOLDS: AtomicBool = AtomicBool::new(false);

        fn holder_took_a_released_permit() {
            B_HOLDS.store(false, Ordering::SeqCst);
            let s = Arc::new(sem(1));
            let a = {
                let s = Arc::clone(&s);
                thread::spawn(move || {
                    s.acquire_blocking(1).unwrap();
                    s.release(1);
                })
            };
            let b = {
                let s = Arc::clone(&s);
                thread::spawn(move || {
                    s.acquire_blocking(1).unwrap();
                    B_HOLDS.store(true, Ordering::SeqCst);
                    thread::yield_now();
                    B_HOLDS.store(false, Ordering::SeqCst);
                    s.release(1);
                })
            };
            match s.try_acquire(1) {
                Ok(()) => s.release(1),
                Err(_) if B_HOLDS.load(Ordering::SeqCst) => {
                    *FAILURE_CLOCK.lock().unwrap() = current::clock().to_vec();
                    panic!("expected panic");
                }
                Err(_) => {}
            }
            a.join().unwrap();
            b.join().unwrap();
        }

        assert_eq!(
            replay_up_to_the_failure_clock(holder_took_a_released_permit, &FAILURE_CLOCK),
            "expected panic"
        );
    }

    /// `has_waiters` read on its own, not right after `load_permits`, is causally after the acquire
    /// that made the waiter park, and not only after the park: a replay up to its clock must not
    /// leave out the holder's acquire, or the waiter takes the permit instead of parking, and the
    /// replay ends before the failure. The holder makes an event of its own first, so that its
    /// acquire is not already in the waiter's past.
    #[test_log::test]
    fn has_waiters_is_after_the_holder_of_the_permits() {
        static FAILURE_CLOCK: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

        fn waiter_parked_behind_another_task() {
            let s = Arc::new(sem(1));
            let holder = {
                let s = Arc::clone(&s);
                thread::spawn(move || {
                    // An event of this task first, as for a task that did something before.
                    BatchSemaphore::new(0, Fairness::Unfair).release(1);
                    if s.try_acquire(1).is_ok() {
                        thread::yield_now();
                        s.release(1);
                    }
                })
            };
            let waiter = {
                let s = Arc::clone(&s);
                thread::spawn(move || {
                    s.acquire_blocking(1).unwrap();
                    s.release(1);
                })
            };
            thread::yield_now();
            if s.has_waiters() {
                *FAILURE_CLOCK.lock().unwrap() = current::clock().to_vec();
                panic!("expected panic");
            }
            holder.join().unwrap();
            waiter.join().unwrap();
        }

        assert_eq!(
            replay_up_to_the_failure_clock(waiter_parked_behind_another_task, &FAILURE_CLOCK),
            "expected panic"
        );
    }
}
