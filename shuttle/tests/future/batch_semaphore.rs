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
    for fairness in [Fairness::StrictlyFair, Fairness::Unfair] {
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
    for fairness in [Fairness::StrictlyFair, Fairness::Unfair] {
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
    for fairness in [Fairness::StrictlyFair, Fairness::Unfair] {
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
    for fairness in [Fairness::StrictlyFair, Fairness::Unfair] {
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

    check_dfs(
        || {
            future::block_on(async {
                // The `Acquire` outlives the task that first polls it. A stdlib
                // mutex keeps the handoff free of extra yield points.
                let acquire = Arc::new(Mutex::new(Some(Box::pin(SEM.acquire(1)))));
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
                        PARK.acquire(1).await.unwrap();
                    }));
                    // Serialize the two polls: the second task must be the last
                    // one to have polled when the release below happens.
                    polled_rx.recv().unwrap();
                }

                // Grants the permit to the queued waiter, which must notify the
                // task that polled most recently rather than the one that first
                // registered.
                SEM.release(1);
                assert!(recorder_b.woken(), "second poller was not woken");
                assert!(!recorder_a.woken(), "first poller was woken instead");

                PARK.release(2);
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
/// future, and polling it from a task that *is* running has to succeed.
#[test]
fn release_to_waiter_of_finished_task() {
    // A `lazy_static` semaphore gives the `Acquire` a `'static` borrow, so it can
    // be handed to a `'static` spawned task and outlive it.
    shuttle::lazy_static! {
        static ref SEM: BatchSemaphore = BatchSemaphore::new(0, Fairness::StrictlyFair);
    }

    check_dfs(
        || {
            future::block_on(async {
                // A stdlib mutex keeps the handoff free of extra yield points.
                let acquire = Arc::new(Mutex::new(None));

                let acquire2 = Arc::clone(&acquire);
                let registrant = future::spawn(async move {
                    // Created *and* first polled by this task, so the queued
                    // waiter belongs to the task that is about to finish.
                    let mut acq = Box::pin(SEM.acquire(1));
                    // No permits are available, so this leaves a waiter queued.
                    assert!(futures::poll!(acq.as_mut()).is_pending());
                    // Hand the still-queued acquire back out so it outlives this
                    // task, then finish without polling or dropping it again.
                    *acquire2.lock().unwrap() = Some(acq);
                });
                registrant.await.unwrap();

                // The only queued waiter belongs to a task that has finished.
                SEM.release(1);
                assert_eq!(
                    SEM.available_permits(),
                    1,
                    "permits were consumed by a waiter nobody is waiting on"
                );

                // The abandoned acquire is still usable by a live task.
                let acq = acquire.lock().unwrap().take().unwrap();
                acq.await.unwrap();
                assert_eq!(SEM.available_permits(), 0);
            });
        },
        None,
    );
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

/// A task that polls more than one `Acquire` can own a queued waiter that cannot make progress.
/// When the task then takes permits of the unfair semaphore, the semaphore must not block it: the
/// task is running, and blocks itself if it waits for that `Acquire`. Blocking it here made the
/// next scheduling point a false deadlock.
#[test]
fn acquiring_does_not_block_the_running_task() {
    check_dfs(
        || {
            future::block_on(async {
                let sem = BatchSemaphore::new(1, Fairness::Unfair);
                let mut waiting = Box::pin(sem.acquire(2));
                assert!(futures::poll!(waiting.as_mut()).is_pending());
                sem.try_acquire(1).unwrap();
                thread::yield_now();
                sem.release(1);
                drop(waiting);
            });
        },
        None,
    )
}

/// Tests of `BatchSemaphore::acquire_reserving`, and of `upgrade` on an unfair semaphore, which
/// reserves the semaphore too.
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

mod reservation_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// Whether an event of `task` happens before the current point of the current task.
    fn after(task: usize) -> bool {
        let clock = current::clock();
        let times: &[u32] = &clock;
        times.get(task).is_some_and(|&time| time > 0)
    }

    /// Until `min_permits` permits are available, a reserving acquire holds nothing and stops no
    /// other request. Here the main task holds 3 of 4 permits, so the reserver, which reserves at
    /// 2, waits, and the main task still gets the last permit.
    #[test_log::test]
    fn waits_without_reserving_below_min_permits() {
        check_dfs(
            || {
                let sem = Arc::new(BatchSemaphore::new(4, Fairness::Unfair));
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

    /// Once `min_permits` permits are available, the request reserves the semaphore: no other
    /// request can take a permit until the reservation is granted, although permits are free.
    #[test_log::test]
    fn reservation_keeps_permits_from_other_requests() {
        check_dfs(
            || {
                future::block_on(async {
                    let sem = Arc::new(BatchSemaphore::new(4, Fairness::Unfair));
                    // The main task holds one permit, like a reader of an `RwLock`.
                    sem.acquire(1).await.unwrap();
                    let mut reserve = Box::pin(sem.acquire_reserving(2, 4));
                    // 3 permits are available, which is at least 2, so this reserves the semaphore.
                    assert!(futures::poll!(reserve.as_mut()).is_pending());
                    assert_eq!(sem.available_permits(), 0);

                    let granted = Arc::new(AtomicBool::new(false));
                    let other = future::spawn({
                        let (sem, granted) = (sem.clone(), granted.clone());
                        async move {
                            if sem.try_acquire(1).is_ok() {
                                assert!(granted.load(Ordering::SeqCst), "try_acquire took a reserved permit");
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

    /// The reserver takes the permits that the holders release, so it is after each holder that
    /// released, and not after a task that was not let in before it.
    #[test_log::test]
    fn reserver_is_after_the_releases_it_waits_for() {
        check_dfs(
            || {
                let sem = Arc::new(BatchSemaphore::new(4, Fairness::Unfair));
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

    /// Only one request can hold the reservation. A second reserving request waits until the
    /// first is granted, although on its own (with `min_permits` 0) it would reserve at once.
    #[test_log::test]
    fn one_reservation_at_a_time() {
        check_dfs(
            || {
                future::block_on(async {
                    let sem = Arc::new(BatchSemaphore::new(4, Fairness::Unfair));
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

    /// Dropping a reserving acquire that holds the reservation ends the reservation, and the
    /// permits it kept are available again, also to a task that already waits for them.
    #[test_log::test]
    fn dropped_reservation_frees_the_permits() {
        check_dfs(
            || {
                future::block_on(async {
                    let sem = Arc::new(BatchSemaphore::new(2, Fairness::Unfair));
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
                    drop(reserve);
                    other.await.unwrap();
                    sem.release(1);
                    assert_eq!(sem.available_permits(), 2);
                });
            },
            None,
        );
    }

    /// Closing the semaphore wakes the holder of the reservation, which then fails.
    #[test_log::test]
    fn close_wakes_the_reserver() {
        check_dfs(
            || {
                future::block_on(async {
                    let sem = Arc::new(BatchSemaphore::new(2, Fairness::Unfair));
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

    /// A reservation whose task finished without dropping its `Acquire` must not keep the permits
    /// from everyone else. A release drops it, and the abandoned `Acquire` still works when a live
    /// task polls it (see `release_to_waiter_of_finished_task`).
    #[test_log::test]
    fn reservation_of_finished_task_is_dropped() {
        shuttle::lazy_static! {
            static ref SEM: BatchSemaphore = BatchSemaphore::new(2, Fairness::Unfair);
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

    /// On an unfair semaphore, an upgrade reserves the semaphore at once: new requests wait until
    /// the upgrade is granted, and the upgrade waits only for the permits that are held.
    #[test_log::test]
    fn unfair_upgrade_reserves_the_semaphore() {
        check_dfs(
            || {
                future::block_on(async {
                    let sem = Arc::new(BatchSemaphore::new(4, Fairness::Unfair));
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

    /// A request that will reserve the semaphore changes its state, so it gets a scheduling point
    /// before it reserves, like a request that will be granted. Without one, the reservation would
    /// happen in the same step as whatever the task did before it asked, and no other task could run
    /// in between. Here the reserver sets a flag, then asks while the main task holds a permit, so it
    /// reserves and waits. Some schedule must let another task see the flag and still take a permit,
    /// as a `parking_lot` reader can between a writer's earlier action and its `WRITER_BIT`.
    #[test_log::test]
    fn scheduling_point_before_reserving() {
        static TOOK_PERMIT_AFTER_FLAG: AtomicBool = AtomicBool::new(false);
        check_dfs(
            || {
                let sem = Arc::new(BatchSemaphore::new(4, Fairness::Unfair));
                sem.acquire_blocking(1).unwrap();
                let flag = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
                let reserver = {
                    let (sem, flag) = (sem.clone(), flag.clone());
                    thread::spawn(move || {
                        flag.store(true, Ordering::SeqCst);
                        // 3 permits are available, which is at least 2, so this reserves.
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
}

/// Tests of `BatchSemaphore::release_fair`: a release that grants already waiting requests their
/// permits inside the release itself, so that no other request can overtake them, like
/// `parking_lot`'s fair unlock.
mod fair_release_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// A waiter granted by a fair release is causally after the release, whether the grant happens
    /// inside the release (the hand-off, which assigns the permits' clock to the waiter there) or
    /// the waiter acquires on its own poll. The sibling tasks share no other causality, so the
    /// clock edge can only come from the grant. `batch_semaphore_clock_1` is the plain-release
    /// equivalent.
    #[test_log::test]
    fn fair_release_clock() {
        check_dfs(
            || {
                let s = Arc::new(BatchSemaphore::new(0, Fairness::Unfair));

                let s2 = s.clone();
                thread::spawn(move || {
                    assert_eq!(me(), 1);
                    s2.release_fair(1);
                });
                thread::spawn(move || {
                    assert_eq!(me(), 2);
                    check_clock(|i, c| (i != 1) || (c == 0));
                    s.acquire_blocking(1).unwrap();
                    // after the acquire, we are causally dependent on task 1's release
                    check_clock(|i, c| (i != 1) || (c > 0));
                });
            },
            None,
        );
    }

    /// A fair release grants a queued waiter its permits inside the release: afterwards the permits
    /// are gone, with no scheduling point in between at which anything could have barged. A plain
    /// release instead leaves the permits available and lets every request race for them.
    #[test_log::test]
    fn fair_release_grants_the_queued_waiter_inside_the_release() {
        check_dfs(
            || {
                future::block_on(async {
                    let sem = BatchSemaphore::new(1, Fairness::Unfair);
                    sem.acquire(1).await.unwrap();
                    let mut waiting = Box::pin(sem.acquire(1));
                    assert!(futures::poll!(waiting.as_mut()).is_pending());

                    sem.release_fair(1);
                    // No scheduling point separates the release from this read, so only the grant
                    // inside the release can have taken the permit.
                    assert_eq!(sem.available_permits(), 0, "the waiter was not granted the permit");
                    waiting.as_mut().await.unwrap();
                    sem.release(1);
                });
            },
            None,
        );
    }

    /// The contrast to the test above, pinning down what plain `release` does on an unfair
    /// semaphore: the permits stay available, and the releasing task itself can take them back
    /// ahead of the queued waiter.
    #[test_log::test]
    fn plain_release_lets_the_releaser_barge_past_the_waiter() {
        check_dfs(
            || {
                future::block_on(async {
                    let sem = BatchSemaphore::new(1, Fairness::Unfair);
                    sem.acquire(1).await.unwrap();
                    let mut waiting = Box::pin(sem.acquire(1));
                    assert!(futures::poll!(waiting.as_mut()).is_pending());

                    sem.release(1);
                    // The permit is up for grabs rather than granted: any request could now take
                    // it ahead of the waiter (`fair_release_hands_off_to_a_waiting_task` shows the
                    // race across tasks).
                    assert_eq!(sem.available_permits(), 1, "a plain release must not hand off");
                    waiting.as_mut().await.unwrap();
                    sem.release(1);
                });
            },
            None,
        );
    }

    /// A fair release grants from the front of the queue, for as long as the permits last: the
    /// longest-waiting request first, and a request that does not fit stops the scan, like the
    /// first parked writer stops `parking_lot`'s wake-up. What is left over is free for anyone.
    #[test_log::test]
    fn fair_release_grants_from_the_front_until_the_permits_run_out() {
        check_dfs(
            || {
                future::block_on(async {
                    let sem = BatchSemaphore::new(3, Fairness::Unfair);
                    sem.acquire(3).await.unwrap();
                    let mut first = Box::pin(sem.acquire(2));
                    assert!(futures::poll!(first.as_mut()).is_pending());
                    let mut second = Box::pin(sem.acquire(2));
                    assert!(futures::poll!(second.as_mut()).is_pending());

                    sem.release_fair(3);
                    // The first waiter took 2 inside the release; the second does not fit the 1
                    // that is left, which stays available rather than earmarked.
                    assert_eq!(sem.available_permits(), 1);
                    assert!(futures::poll!(first.as_mut()).is_ready());
                    assert!(futures::poll!(second.as_mut()).is_pending());

                    // The first waiter leaves, and the second takes its permits.
                    sem.release(2);
                    second.as_mut().await.unwrap();
                    sem.release(2);
                });
            },
            None,
        );
    }

    /// While a reservation holds the semaphore, a fair release changes nothing about who is next:
    /// the permits are kept for the reservation's holder, not handed to the queue.
    #[test_log::test]
    fn fair_release_defers_to_the_reservation() {
        check_dfs(
            || {
                future::block_on(async {
                    let sem = BatchSemaphore::new(2, Fairness::Unfair);
                    sem.acquire(1).await.unwrap();
                    let mut reserve = Box::pin(sem.acquire_reserving(1, 2));
                    assert!(futures::poll!(reserve.as_mut()).is_pending());
                    let mut waiting = Box::pin(sem.acquire(1));
                    assert!(futures::poll!(waiting.as_mut()).is_pending());

                    sem.release_fair(1);
                    assert_eq!(sem.available_permits(), 0);
                    assert!(
                        futures::poll!(reserve.as_mut()).is_ready(),
                        "the reservation's holder did not get the permits"
                    );
                    assert!(futures::poll!(waiting.as_mut()).is_pending());

                    sem.release(2);
                    waiting.as_mut().await.unwrap();
                    sem.release(1);
                });
            },
            None,
        );
    }

    /// On a strictly fair semaphore every release already grants from the front of the queue, so
    /// `release_fair` is the same as `release`.
    #[test_log::test]
    fn fair_release_on_a_strictly_fair_semaphore_is_a_release() {
        check_dfs(
            || {
                future::block_on(async {
                    let sem = BatchSemaphore::new(1, Fairness::StrictlyFair);
                    sem.acquire(1).await.unwrap();
                    let mut waiting = Box::pin(sem.acquire(1));
                    assert!(futures::poll!(waiting.as_mut()).is_pending());

                    sem.release_fair(1);
                    assert_eq!(sem.available_permits(), 0);
                    waiting.as_mut().await.unwrap();
                    sem.release(1);
                });
            },
            None,
        );
    }

    /// The hand-off across tasks: whenever the waiter's request is queued at the moment of the fair
    /// release, the waiter gets the permit inside the release (`available_permits` is 0 with no
    /// scheduling point in between). Both that case and the one where the waiter had not asked yet
    /// must show up across the schedules.
    #[test_log::test]
    fn fair_release_hands_off_to_a_waiting_task() {
        static SAW_HANDOFF: AtomicBool = AtomicBool::new(false);
        static SAW_FREE: AtomicBool = AtomicBool::new(false);
        check_dfs(
            || {
                let sem = Arc::new(BatchSemaphore::new(1, Fairness::Unfair));
                sem.acquire_blocking(1).unwrap();
                let waiter = {
                    let sem = sem.clone();
                    thread::spawn(move || {
                        sem.acquire_blocking(1).unwrap();
                        sem.release(1);
                    })
                };
                sem.release_fair(1);
                // No scheduling point since the release: 0 means the waiter was queued and was
                // granted the permit inside the release; 1 means it had not asked yet.
                match sem.available_permits() {
                    0 => SAW_HANDOFF.store(true, Ordering::SeqCst),
                    1 => SAW_FREE.store(true, Ordering::SeqCst),
                    n => panic!("impossible permit count {n}"),
                }
                waiter.join().unwrap();
            },
            None,
        );
        assert!(SAW_HANDOFF.load(Ordering::SeqCst), "no schedule had a queued waiter");
        assert!(
            SAW_FREE.load(Ordering::SeqCst),
            "no schedule had the waiter still to ask"
        );
    }

    /// A fair release hands a queued reserving waiter the reservation when it can reserve but not
    /// yet take all of its permits, as `parking_lot` hands a writer `WRITER_BIT` while readers are
    /// left. A request that queued after it can then not overtake it.
    #[test_log::test]
    fn fair_release_hands_the_reservation_to_the_front_waiter() {
        check_dfs(
            || {
                future::block_on(async {
                    let sem = BatchSemaphore::new(3, Fairness::Unfair);
                    // Like an upgradable reader (2 permits) and a plain reader (1).
                    sem.acquire(2).await.unwrap();
                    sem.acquire(1).await.unwrap();
                    let mut writer = Box::pin(sem.acquire_reserving(2, 3));
                    assert!(futures::poll!(writer.as_mut()).is_pending());
                    let mut later = Box::pin(sem.acquire(1));
                    assert!(futures::poll!(later.as_mut()).is_pending());

                    sem.release_fair(2);
                    assert_eq!(sem.available_permits(), 0, "the writer was not handed the reservation");
                    assert!(
                        futures::poll!(later.as_mut()).is_pending(),
                        "a later request overtook the writer"
                    );
                    assert!(futures::poll!(writer.as_mut()).is_pending());

                    // The last reader leaves, and the writer takes every permit.
                    sem.release(1);
                    assert!(futures::poll!(writer.as_mut()).is_ready());
                    assert!(futures::poll!(later.as_mut()).is_pending());
                    sem.release(3);
                    later.as_mut().await.unwrap();
                    sem.release(1);
                });
            },
            None,
        );
    }

    /// On a semaphore built `with_fair_releases`, every `release` is fair: it grants the queued
    /// waiter its permits inside the release, like `release_fair`.
    #[test_log::test]
    #[allow(deprecated)] // `with_fair_releases` is deprecated so that only `shuttle-parking_lot` uses it.
    fn with_fair_releases_makes_release_fair() {
        check_dfs(
            || {
                future::block_on(async {
                    let sem = BatchSemaphore::new(1, Fairness::Unfair).with_fair_releases();
                    sem.acquire(1).await.unwrap();
                    let mut waiting = Box::pin(sem.acquire(1));
                    assert!(futures::poll!(waiting.as_mut()).is_pending());

                    sem.release(1);
                    assert_eq!(sem.available_permits(), 0, "the waiter was not granted the permit");
                    waiting.as_mut().await.unwrap();
                    sem.release(1);
                });
            },
            None,
        );
    }

    /// On a semaphore built `with_fair_releases`, the order of the queue decides who gets the
    /// permits, so Shuttle must explore every order in which tasks can join it. T2 asks for the
    /// permit only after T1's message, and the main task releases it only after T2's message, so
    /// both ask while the main task holds it. Without a scheduling point between T1's message and T1
    /// joining the queue, T1 would always be first in the queue, and get the permit first.
    #[test_log::test]
    #[allow(deprecated)] // `with_fair_releases` is deprecated so that only `shuttle-parking_lot` uses it.
    fn with_fair_releases_explores_every_queue_order() {
        let orders = Arc::new(Mutex::new(HashSet::new()));
        let orders_clone = Arc::clone(&orders);
        check_dfs(
            move || {
                let sem = Arc::new(BatchSemaphore::new(1, Fairness::Unfair).with_fair_releases());
                sem.acquire_blocking(1).unwrap();
                let order = Arc::new(Mutex::new(Vec::new()));
                let (to_second, from_first) = shuttle::sync::mpsc::channel();
                let (to_main, from_second) = shuttle::sync::mpsc::channel();
                let first = {
                    let (sem, order) = (Arc::clone(&sem), Arc::clone(&order));
                    thread::spawn(move || {
                        to_second.send(()).unwrap();
                        sem.acquire_blocking(1).unwrap();
                        order.lock().unwrap().push(1);
                        sem.release(1);
                    })
                };
                let second = {
                    let (sem, order) = (Arc::clone(&sem), Arc::clone(&order));
                    thread::spawn(move || {
                        from_first.recv().unwrap();
                        to_main.send(()).unwrap();
                        sem.acquire_blocking(1).unwrap();
                        order.lock().unwrap().push(2);
                        sem.release(1);
                    })
                };
                from_second.recv().unwrap();
                sem.release(1);
                first.join().unwrap();
                second.join().unwrap();
                orders_clone.lock().unwrap().insert(order.lock().unwrap().clone());
            },
            None,
        );
        let orders = orders.lock().unwrap();
        assert_eq!(*orders, HashSet::from([vec![1, 2], vec![2, 1]]));
    }
}

/// Tests of `BatchSemaphore::load_permits`: a read of the semaphore's state with one scheduling
/// point and no effect.
mod load_permits_tests {
    use super::*;

    /// `load_permits` reports the permits a request could take now: the free permits, zero while a
    /// reservation lasts, and `None` once the semaphore is closed.
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
    /// instead would transiently take the permit, which a concurrent `try_acquire` could observe;
    /// `cannot_fail` below rules that out.
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
}
