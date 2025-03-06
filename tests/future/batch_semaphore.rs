use crate::basic::clocks::{check_clock, me};
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use shuttle::future::{self, batch_semaphore::*};
use shuttle::{check_dfs, check_random, current, thread};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
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

// Create a semaphore with `num_permits` permits and spawn a bunch of tasks that each
// try to grab a bunch of permits.  Task i sets the i'th bit in a shared atomic counter.
// Afterwards, we'll see which combinations were allowable over a full dfs run.
async fn semtest(num_permits: usize, counts: Vec<usize>, states: &Arc<Mutex<HashSet<(usize, usize)>>>, mode: Fairness) {
    let s = Arc::new(BatchSemaphore::new(num_permits, mode));
    let r = Arc::new(AtomicUsize::new(0));
    let mut handles = vec![];
    for (i, &c) in counts.iter().enumerate() {
        let s = s.clone();
        let r = r.clone();
        let states = states.clone();
        let val = 1usize << i;
        handles.push(future::spawn(async move {
            s.acquire(c).await.unwrap();
            let v = r.fetch_add(val, Ordering::SeqCst);
            future::yield_now().await;
            let _ = r.fetch_sub(val, Ordering::SeqCst);
            states.lock().unwrap().insert((i, v));
            s.release(c);
        }));
    }
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
        Future,
        future::join_all,
        task::{Context, Poll, Waker},
    };
    use pin_project::pin_project;
    use proptest::prelude::*;
    use proptest_derive::Arbitrary;
    use shuttle::{
        check_random,
        sync::mpsc::{Sender, channel},
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
        ($mod_name:ident, $fairness:expr_2021) => {
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
