use crate::basic::clocks::{check_clock, me};
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use shuttle::future::{self, batch_semaphore::*};
use shuttle::{check_dfs, current, thread};
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use test_log::test;

#[test]
fn batch_semaphore_basic() {
    check_dfs(
        || {
            let s = BatchSemaphore::new(3);

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

#[test]
fn batch_semaphore_clock_1() {
    check_dfs(
        || {
            let s = Arc::new(BatchSemaphore::new(0));

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

#[test]
fn batch_semaphore_clock_2() {
    check_dfs(
        || {
            let s = Arc::new(BatchSemaphore::new(0));

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

#[test]
fn batch_semaphore_clock_3() {
    check_dfs(
        || {
            let s = Arc::new(BatchSemaphore::new(0));

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

#[test]
fn batch_semaphore_clock_4() {
    check_dfs(
        || {
            let s = Arc::new(BatchSemaphore::new(1));

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
async fn semtest(num_permits: usize, counts: Vec<usize>, states: &Arc<Mutex<HashSet<(usize, usize)>>>) {
    let s = Arc::new(BatchSemaphore::new(num_permits));
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
                semtest(5, vec![3, 3, 3], &states2).await;
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
                semtest(5, vec![3, 3, 2], &states2).await;
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
            let sem = Arc::new(BatchSemaphore::new(0));
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
                let tx = Arc::new(BatchSemaphore::new(1));
                let rx = Arc::new(BatchSemaphore::new(0));
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
                let sem = Arc::new(BatchSemaphore::new(0));
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
            let sem = Arc::new(BatchSemaphore::new(1));
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

// This test exercises the following scenario where SEM is a BatchSemaphore with N permits.
// 1. Initially the semaphore has 0 permits
// 2. Task T1 tries to acquire 1 permit, gets added as the first Waiter
// 3. Task T2 tries to acquire 1 permit, gets added as the second Waiter
// 4. Task T0 releases 1 permit
// 5. Task T1 drops its Acquire handle without calling poll()
//
// At this point, T2 should be woken up and should get the permit.
// Unfortunately, in an earlier version of BatchSemaphore, this was not happening because
// the Drop handler for Acquire was only returning permits to the semaphore, but not
// waking up the next waiter in the queue.
//
// The tests below exercise both the specific scenario above (with 1 permit and two tasks), and more
// general scenarios involving multiple tasks, of which a random subset drop the Acquire guards early.
mod early_acquire_drop_test {
    use super::*;
    use futures::{
        future::join_all,
        task::{Context, Poll, Waker},
        Future,
    };
    use pin_project::pin_project;
    use proptest::proptest;
    use shuttle::{
        check_random,
        sync::mpsc::{channel, Sender},
    };
    use std::pin::Pin;
    use test_log::test;

    #[pin_project]
    struct Task {
        poll_count: usize, // how many times the Future has been polled
        early_drop: bool,  // whether to drop the Acquire handle after polling it once
        tx: Sender<Waker>, // channel for informing the main task
        #[pin]
        acquire: Acquire<'static>,
    }

    impl Task {
        fn new(early_drop: bool, tx: Sender<Waker>, sem: &'static BatchSemaphore) -> Self {
            Self {
                poll_count: 0,
                early_drop,
                tx,
                acquire: sem.acquire(1),
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
            } else if *this.early_drop {
                // Since this is an early drop, we got 0 permits
                Poll::Ready(0)
            } else {
                // If not early dropping, wait until the inner Acquire handle successfully gets
                // a permit.  When successful, return 1 permit.
                this.acquire.as_mut().poll(cx).map(|_| 1)
            }
        }
    }

    // High-level sketch of the test:
    // S1. Initialize the semaphore with no permits
    // S2. Spawn a set of tasks, each randomly decides whether or not to drop early
    //     Each task creates an Acquire handle and polls it once (to get into the waiter queue)
    //     The task then notifies the main task (by sending a message on an mpsc channel)
    // S3. The main task waits for messages from all the spawned tasks (so it knows each is a waiter)
    // S4. The main task releases N permits on the BatchSemaphore, and wakes up all the tasks
    // S5. At this point, each task either drops its Acquire handle, or tries to acquire the BatchSemaphore
    //     by polling it until it acquires a permit.
    fn dropped_acquire_must_release(num_permits: usize, early_drop: Vec<bool>) {
        shuttle::lazy_static! {
            // S1. Initialize the semaphore with no permits
            static ref SEM: BatchSemaphore = BatchSemaphore::new(0);
        }

        future::block_on(async move {
            let mut wakers = vec![];
            let mut handles = vec![];

            // S2. Main task spawns a set of tasks; the `early_drop` vector of booleans determines
            // which tasks will drop the `Acquire` after polling it exactly once
            for early_drop in early_drop {
                let (tx, rx) = channel();
                let task: Task = Task::new(early_drop, tx, &SEM);
                handles.push(future::spawn(async move {
                    let p = task.await;
                    // Note: tasks doing an early drop will return p=0, and release(0) is a no-op
                    SEM.release(p);
                }));
                // S3. Main task waits for message from spawned task indicating it has polled once
                wakers.push(rx.recv().unwrap());
            }

            // S4. Main task releases N permits and wakes up all tasks
            SEM.release(num_permits);
            for w in wakers.into_iter() {
                w.wake();
            }

            join_all(handles).await;
        });
    }

    // The minimal test case (generated by the proptest below) is with 1 permit and 2 tasks, where Task1 does
    // an early drop, and Task2 does not early drop.  This test checks that scenario exhaustively using check_dfs.
    #[test]
    fn dropped_acquire_must_release_exhaustive() {
        check_dfs(|| dropped_acquire_must_release(1, vec![true, false]), None);
    }

    // This test checks scenarios where the main task releases multiple permits and there are several tasks, any
    // subset of which may do an early drop of their Acquire handle.

    const MAX_PERMITS: usize = 8;
    const MAX_TASKS: usize = 7;

    proptest! {
        #[test]
        fn dropped_acquire_must_release_random(num_permits in 1..MAX_PERMITS, early_drop in proptest::collection::vec(proptest::arbitrary::any::<bool>(), 1..MAX_TASKS)) {
            check_random(
                move || dropped_acquire_must_release(num_permits, early_drop.clone()),
                10_000,
            );
        }
    }
}
