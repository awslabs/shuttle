use futures::channel::oneshot;
use futures::task::{FutureObj, Spawn, SpawnError, SpawnExt as _};
use futures::{try_join, Future};
use shuttle::sync::{Barrier, Mutex};
use shuttle::{check_dfs, check_random, future, scheduler::PctScheduler, thread, Runner};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use test_log::test;

async fn add(a: u32, b: u32) -> u32 {
    a + b
}

#[test]
fn async_fncall() {
    check_dfs(
        move || {
            let sum = add(3, 5);
            future::spawn(async move {
                let r = sum.await;
                assert_eq!(r, 8u32);
            });
        },
        None,
    );
}

#[test]
fn async_with_join() {
    check_dfs(
        move || {
            thread::spawn(|| {
                let join = future::spawn(async move { add(10, 32).await });

                future::spawn(async move {
                    assert_eq!(join.await.unwrap(), 42u32);
                });
            });
        },
        None,
    );
}

#[test]
fn async_with_threads() {
    check_dfs(
        move || {
            thread::spawn(|| {
                let v1 = async { 3u32 };
                let v2 = async { 2u32 };
                future::spawn(async move {
                    assert_eq!(5u32, v1.await + v2.await);
                });
            });
            thread::spawn(|| {
                let v1 = async { 5u32 };
                let v2 = async { 6u32 };
                future::spawn(async move {
                    assert_eq!(11u32, v1.await + v2.await);
                });
            });
        },
        None,
    );
}

#[test]
fn async_block_on() {
    check_dfs(
        || {
            let v = future::block_on(async { 42u32 });
            assert_eq!(v, 42u32);
        },
        None,
    );
}

#[test]
fn async_spawn() {
    check_dfs(
        || {
            let t = future::spawn(async { 42u32 });
            let v = future::block_on(async { t.await.unwrap() });
            assert_eq!(v, 42u32);
        },
        None,
    );
}

#[test]
fn async_spawn_chain() {
    check_dfs(
        || {
            let t1 = future::spawn(async { 1u32 });
            let t2 = future::spawn(async move { t1.await.unwrap() });
            let v = future::block_on(async move { t2.await.unwrap() });
            assert_eq!(v, 1u32);
        },
        None,
    );
}

#[test]
fn async_thread_yield() {
    // This tests if thread::yield_now can be called from within an async block
    check_dfs(
        || {
            future::spawn(async move {
                thread::yield_now();
            });
            future::spawn(async move {});
        },
        None,
    )
}

#[test]
#[should_panic(expected = "DFS should find a schedule where r=1 here")]
fn async_atomic() {
    // This tests if shuttle can correctly schedule a different task
    // after thread::yield_now is called from within an async block
    use std::sync::atomic::{AtomicUsize, Ordering};
    check_dfs(
        || {
            let r = Arc::new(AtomicUsize::new(0));
            let r1 = r.clone();
            future::spawn(async move {
                r1.store(1, Ordering::SeqCst);
                thread::yield_now();
                r1.store(0, Ordering::SeqCst);
            });
            future::spawn(async move {
                assert_eq!(r.load(Ordering::SeqCst), 0, "DFS should find a schedule where r=1 here");
            });
        },
        None,
    )
}

#[test]
fn async_mutex() {
    // This test checks that futures can acquire Shuttle sync mutexes.
    // The future should only block on acquiring a lock when
    // another task holds the lock.
    check_dfs(
        move || {
            let lock = Arc::new(Mutex::new(0u64));

            let t1 = {
                let lock = Arc::clone(&lock);
                future::spawn(async move {
                    let mut l = lock.lock().unwrap();
                    *l += 1;
                })
            };

            let t2 = future::block_on(async move {
                t1.await.unwrap();
                *lock.lock().unwrap()
            });

            assert_eq!(t2, 1);
        },
        None,
    )
}

#[test]
fn async_yield() {
    check_dfs(
        || {
            let v = future::block_on(async {
                future::yield_now().await;
                42u32
            });
            assert_eq!(v, 42u32);
        },
        None,
    )
}

#[test]
fn join_handle_abort() {
    check_dfs(
        || {
            let counter = Arc::new(AtomicUsize::new(0));
            let t1 = future::spawn({
                let counter = Arc::clone(&counter);
                async move {
                    Barrier::new(2).wait();
                    counter.fetch_add(1, Ordering::SeqCst)
                }
            });
            t1.abort();
            t1.abort(); // should be safe to call it twice
            assert_eq!(0, counter.load(Ordering::SeqCst));
        },
        None,
    );
}

fn async_counter() {
    let counter = Arc::new(AtomicUsize::new(0));

    let tasks: Vec<_> = (0..10)
        .map(|_| {
            let counter = Arc::clone(&counter);
            future::spawn(async move {
                let c = counter.load(Ordering::SeqCst);
                future::yield_now().await;
                counter.fetch_add(c, Ordering::SeqCst);
            })
        })
        .collect();

    future::block_on(async move {
        for t in tasks {
            t.await.unwrap();
        }
    });
}

#[test]
fn async_counter_random() {
    check_random(async_counter, 5000)
}

#[test]
fn async_counter_pct() {
    let scheduler = PctScheduler::new(2, 5000);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(async_counter);
}

async fn do_err(e: bool) -> Result<(), ()> {
    if e {
        Err(())
    } else {
        Ok(())
    }
}

// Check that try_join on Shuttle futures works as expected
#[test]
fn test_try_join() {
    check_dfs(
        || {
            let f2 = do_err(true);
            let f1 = do_err(false);
            let res = future::block_on(async { try_join!(f1, f2) });
            assert!(res.is_err());
        },
        None,
    );
}

// Check that a task may not run if its JoinHandle is dropped
#[test]
fn drop_shuttle_future() {
    let orderings = Arc::new(AtomicUsize::new(0));
    let async_accesses = Arc::new(AtomicUsize::new(0));
    let orderings_clone = orderings.clone();
    let async_accesses_clone = async_accesses.clone();

    check_dfs(
        move || {
            orderings.fetch_add(1, Ordering::SeqCst);
            let async_accesses = async_accesses.clone();
            future::spawn(async move {
                async_accesses.fetch_add(1, Ordering::SeqCst);
            });
        },
        None,
    );

    assert_eq!(2, orderings_clone.load(Ordering::SeqCst));
    assert_eq!(1, async_accesses_clone.load(Ordering::SeqCst));
}

// Same as `drop_shuttle_future`, but the inner task yields first, and might be cancelled part way through
#[test]
fn drop_shuttle_yield_future() {
    let orderings = Arc::new(AtomicUsize::new(0));
    let async_accesses = Arc::new(AtomicUsize::new(0));
    let post_yield_accesses = Arc::new(AtomicUsize::new(0));
    let orderings_clone = orderings.clone();
    let async_accesses_clone = async_accesses.clone();
    let post_yield_accesses_clone = post_yield_accesses.clone();

    check_dfs(
        move || {
            orderings.fetch_add(1, Ordering::SeqCst);
            let async_accesses = async_accesses.clone();
            let post_yield_accesses = post_yield_accesses.clone();
            future::spawn(async move {
                async_accesses.fetch_add(1, Ordering::SeqCst);
                future::yield_now().await;
                post_yield_accesses.fetch_add(1, Ordering::SeqCst);
            });
        },
        None,
    );

    // The three orderings of main task M and spawned task S are:
    // (1) M runs and finished, then S gets dropped and doesn't run
    // (2) M runs, S runs until yield point, then M finishes and S is dropped
    // (3) M runs, S runs until yield point, S runs again and finished, then M finishes
    assert_eq!(3, orderings_clone.load(Ordering::SeqCst));
    assert_eq!(2, async_accesses_clone.load(Ordering::SeqCst));
    assert_eq!(1, post_yield_accesses_clone.load(Ordering::SeqCst));
}

// This test checks two behaviors. First, it checks that a future can be polled twice
// without needing to wake up another task. Second, it checks that a waiter can finish
// before the waitee task.
#[test]
fn wake_self_on_join_handle() {
    check_dfs(
        || {
            let yielder = future::spawn(async move {
                future::yield_now().await;
            });

            struct Timeout<F: Future> {
                inner: Pin<Box<F>>,
                counter: u8,
            }

            impl<F> Future for Timeout<F>
            where
                F: Future,
            {
                type Output = ();

                fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                    if self.counter == 0 {
                        return Poll::Ready(());
                    }

                    self.counter -= 1;

                    match self.inner.as_mut().poll(cx) {
                        Poll::Pending => {
                            cx.waker().wake_by_ref();
                            Poll::Pending
                        }
                        Poll::Ready(_) => Poll::Ready(()),
                    }
                }
            }

            let wait_on_yield = future::spawn(Timeout {
                inner: Box::pin(yielder),
                counter: 2,
            });

            drop(wait_on_yield);
        },
        None,
    );
}

#[test]
fn is_finished_on_join_handle() {
    check_dfs(
        || {
            let barrier = Arc::new(Barrier::new(2));
            let t1 = future::spawn({
                let barrier = Arc::clone(&barrier);
                async move {
                    barrier.wait();
                }
            });
            assert!(!t1.is_finished());

            future::block_on(future::spawn(async move {
                assert!(!t1.is_finished());
                barrier.wait();
                futures::pin_mut!(t1);
                t1.as_mut().await.unwrap();
                assert!(t1.is_finished());
            }))
            .unwrap();
        },
        None,
    );
}

struct ShuttleSpawn;

impl Spawn for ShuttleSpawn {
    fn spawn_obj(&self, future: FutureObj<'static, ()>) -> Result<(), SpawnError> {
        future::spawn(future);
        Ok(())
    }
}

// Make sure a spawned detached task gets cleaned up correctly after execution ends
#[test]
fn clean_up_detached_task() {
    check_dfs(
        || {
            let atomic = shuttle::sync::atomic::AtomicUsize::new(0);
            let _task_handle = ShuttleSpawn
                .spawn_with_handle(async move {
                    atomic.fetch_add(1, Ordering::SeqCst);
                })
                .unwrap();
        },
        None,
    )
}

// --- Task abort tests ---

/// Local mirror of JoinError variants that implements Hash/Eq for use in HashSets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Outcome {
    Cancelled,
    #[allow(dead_code)]
    Other,
}

impl From<shuttle::future::JoinError> for Outcome {
    fn from(e: shuttle::future::JoinError) -> Self {
        match e {
            shuttle::future::JoinError::Cancelled => Outcome::Cancelled,
        }
    }
}

/// Abort a task that has just been spawned. Because abort() is a scheduling
/// point, DFS can explore interleavings where the task completes before the
/// abort flag is set, so both Ok and Cancelled are valid outcomes.
#[test]
fn abort_before_first_poll() {
    use std::collections::HashSet;

    let outcomes: Arc<std::sync::Mutex<HashSet<Result<i32, Outcome>>>> = Default::default();
    let outcomes_clone = outcomes.clone();

    check_dfs(
        move || {
            let t = future::spawn(async move { 42 });
            t.abort();
            let result = future::block_on(t);
            outcomes.lock().unwrap().insert(result.map_err(Outcome::from));
        },
        None,
    );

    let seen = outcomes_clone.lock().unwrap();
    let expected: HashSet<Result<i32, Outcome>> = [Ok(42), Err(Outcome::Cancelled)].into();
    assert_eq!(*seen, expected, "expected both outcomes, saw: {:?}", *seen);
}

/// Abort a task that has already completed. The JoinHandle should return the
/// normal Ok result, not Cancelled.
#[test]
fn abort_already_finished_task() {
    check_dfs(
        || {
            let (tx, rx) = oneshot::channel();
            let t = future::spawn(async move {
                tx.send(()).unwrap();
                42
            });
            // Wait until the task has sent its signal (i.e., it's about to return 42).
            future::block_on(async { rx.await.unwrap() });
            // Task has finished by now. Aborting a finished task should be a no-op.
            t.abort();
            let result = future::block_on(t);
            assert_eq!(result.unwrap(), 42);
        },
        None,
    );
}

/// Abort a task that has started and is holding a Mutex. The abort should cancel the
/// task (returning JoinError::Cancelled), drop the future's locals (releasing the lock),
/// and not execute any code after the await point.
#[test]
fn abort_frees_mutex() {
    check_dfs(
        || {
            let mutex = Arc::new(Mutex::new(()));
            let mutex2 = mutex.clone();
            let after_await = Arc::new(AtomicUsize::new(0));
            let after_clone = after_await.clone();
            let (tx_ready, rx_ready) = oneshot::channel();
            let (tx_proceed, rx_proceed) = oneshot::channel::<()>();

            // Use spawn_local because MutexGuard is !Send across await
            let t = future::spawn_local(async move {
                let _guard = mutex2.lock().unwrap();
                // Signal that we're holding the lock, then sleep forever.
                tx_ready.send(()).unwrap();
                let _ = rx_proceed.await;
                after_clone.fetch_add(1, Ordering::SeqCst);
            });

            // Wait until the task is holding the lock and sleeping.
            future::block_on(async { rx_ready.await.unwrap() });

            // Abort the task that holds the lock
            t.abort();
            let result = future::block_on(t);
            assert!(matches!(result, Err(shuttle::future::JoinError::Cancelled)));
            assert_eq!(after_await.load(Ordering::SeqCst), 0);

            // The lock should have been released by the abort
            let _guard = mutex.lock().unwrap();
            drop(tx_proceed);
        },
        None,
    );
}

/// Abort a future task while it's blocked on a Shuttle sync primitive (Mutex).
/// The abort should not take effect until the sync operation completes and the
/// task reaches an await point. This was a known problem in awslabs/shuttle#150
/// where immediately destroying the task caused Mutex internals to assert-fail.
#[test]
fn abort_while_blocked_on_sync() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(0i32));
            let lock2 = lock.clone();
            let t1 = thread::spawn(move || {
                let mut guard = lock2.lock().unwrap();
                *guard += 1;
            });
            let t2 = future::spawn(async move {
                let _l = lock.lock().unwrap();
            });
            t2.abort();
            t1.join().unwrap();
            let _ = future::block_on(t2);
        },
        None,
    );
}

/// Reproduction case from https://github.com/awslabs/shuttle/issues/91.
/// An aborted task must not run its body even when the future it was awaiting
/// resolves successfully after abort.
#[test]
fn abort_woken_task_does_not_run() {
    check_dfs(
        || {
            let (sender, receiver) = oneshot::channel();
            let t = future::spawn(async move {
                receiver.await.unwrap();
                panic!("should not get here");
            });
            t.abort();
            sender.send(()).unwrap();
            thread::yield_now();
        },
        None,
    );
}

/// Dropping a JoinHandle detaches the task (it keeps running), it does NOT abort.
/// This matches tokio semantics.
#[test]
fn drop_join_handle_detaches_not_aborts() {
    check_dfs(
        || {
            let ran = Arc::new(AtomicUsize::new(0));
            let ran_clone = ran.clone();
            let (tx, rx) = oneshot::channel();

            let t = future::spawn(async move {
                ran_clone.fetch_add(1, Ordering::SeqCst);
                tx.send(()).unwrap();
            });
            drop(t); // detach, not abort
            future::block_on(async { rx.await.unwrap() }); // wait for the task
            assert_eq!(ran.load(Ordering::SeqCst), 1);
        },
        None,
    );
}

/// Abort via AbortHandle works the same as via JoinHandle.
/// Also checks that is_finished() returns true after the aborted task completes.
#[test]
fn abort_via_abort_handle() {
    use std::collections::HashSet;

    let outcomes: Arc<std::sync::Mutex<HashSet<Result<i32, Outcome>>>> = Default::default();
    let outcomes_clone = outcomes.clone();

    check_dfs(
        move || {
            let t = future::spawn(async move { 42 });
            let ah = t.abort_handle();
            ah.abort();
            let result = future::block_on(t);
            // Task is finished regardless of outcome.
            assert!(ah.is_finished());
            outcomes.lock().unwrap().insert(result.map_err(Outcome::from));
        },
        None,
    );

    let seen = outcomes_clone.lock().unwrap();
    let expected: HashSet<Result<i32, Outcome>> = [Ok(42), Err(Outcome::Cancelled)].into();
    assert_eq!(*seen, expected, "expected both outcomes, saw: {:?}", *seen);
}

/// Aborting a task twice is a no-op the second time.
#[test]
fn double_abort_is_idempotent() {
    use std::collections::HashSet;

    let outcomes: Arc<std::sync::Mutex<HashSet<Result<i32, Outcome>>>> = Default::default();
    let outcomes_clone = outcomes.clone();

    check_dfs(
        move || {
            let t = future::spawn(async { 42 });
            t.abort();
            t.abort(); // second abort should be harmless
            let result = future::block_on(t);
            outcomes.lock().unwrap().insert(result.map_err(Outcome::from));
        },
        None,
    );

    let seen = outcomes_clone.lock().unwrap();
    let expected: HashSet<Result<i32, Outcome>> = [Ok(42), Err(Outcome::Cancelled)].into();
    assert_eq!(*seen, expected, "expected both outcomes, saw: {:?}", *seen);
}

/// Abort a task that has multiple await points. check_dfs explores all orderings,
/// and we collect (steps_completed, result) pairs across iterations. The only
/// valid outcomes are:
///   (1, Cancelled) — aborted after step 1, before step 2
///   (2, Cancelled) — aborted after step 2, before step 3
///   (3, Ok)        — task completed before abort took effect
/// Note: (0, Cancelled) is impossible because the ready signal and step 1 are in
/// the same synchronous block (no await between them).
#[test]
fn abort_at_second_await_point() {
    use std::collections::HashSet;
    type StepOutcome = (usize, Result<(), Outcome>);

    let outcomes: Arc<std::sync::Mutex<HashSet<StepOutcome>>> = Default::default();
    let outcomes_clone = outcomes.clone();

    check_dfs(
        move || {
            let steps = Arc::new(AtomicUsize::new(0));
            let steps_clone = steps.clone();
            let (tx_ready, rx_ready) = oneshot::channel();

            let t = future::spawn(async move {
                tx_ready.send(()).unwrap();
                steps_clone.fetch_add(1, Ordering::SeqCst); // step 1
                future::yield_now().await;
                steps_clone.fetch_add(1, Ordering::SeqCst); // step 2
                future::yield_now().await;
                steps_clone.fetch_add(1, Ordering::SeqCst); // step 3
            });

            // Ensure the task has started.
            future::block_on(async { rx_ready.await.unwrap() });

            // Yield to create a scheduling point. check_dfs explores orderings
            // where the task makes different amounts of progress before abort.
            thread::yield_now();

            t.abort();
            let result = future::block_on(t);

            let s = steps.load(Ordering::SeqCst);
            outcomes.lock().unwrap().insert((s, result.map_err(Outcome::from)));
        },
        None,
    );

    let seen = outcomes_clone.lock().unwrap();
    let expected: HashSet<StepOutcome> =
        [(1, Err(Outcome::Cancelled)), (2, Err(Outcome::Cancelled)), (3, Ok(()))].into();
    assert_eq!(*seen, expected, "expected all three outcomes, saw: {:?}", *seen);
}

/// Abort a detached task via AbortHandle. When the JoinHandle is dropped (detaching
/// the task), the AbortHandle should still be able to abort it.
#[test]
fn abort_detached_task_via_abort_handle() {
    check_dfs(
        || {
            let ran = Arc::new(AtomicUsize::new(0));
            let ran_clone = ran.clone();
            let (tx_ready, rx_ready) = oneshot::channel();
            let (tx_proceed, rx_proceed) = oneshot::channel::<()>();

            let t = future::spawn(async move {
                tx_ready.send(()).unwrap();
                let _ = rx_proceed.await;
                ran_clone.fetch_add(1, Ordering::SeqCst);
            });
            let ah = t.abort_handle();
            drop(t); // detach the task

            // Wait until the task is sleeping at the rx_proceed await point.
            future::block_on(async { rx_ready.await.unwrap() });

            ah.abort(); // abort via the AbortHandle

            // Yield so the scheduler can run the aborted (detached) task's cleanup.
            thread::yield_now();

            assert_eq!(ran.load(Ordering::SeqCst), 0);
            drop(tx_proceed);
        },
        None,
    );
}

/// Test that forgetting a JoinHandle (std::mem::forget) doesn't cause issues.
/// The task continues running as if detached.
#[test]
fn forget_join_handle_task_continues() {
    check_dfs(
        || {
            let ran = Arc::new(AtomicUsize::new(0));
            let ran_clone = ran.clone();
            let (tx, rx) = oneshot::channel();

            let t = future::spawn(async move {
                ran_clone.fetch_add(1, Ordering::SeqCst);
                tx.send(()).unwrap();
            });
            std::mem::forget(t); // no Drop runs, task is not detached or aborted
            future::block_on(async { rx.await.unwrap() }); // wait for the task
            assert_eq!(ran.load(Ordering::SeqCst), 1);
        },
        None,
    );
}

/// Destructors of the inner future run when the task is aborted.
/// The DropCounter is dropped exactly once whether the task completes or is cancelled.
#[test]
fn abort_runs_inner_future_drop() {
    use std::collections::HashSet;
    use std::sync::Mutex as StdMutex;

    #[derive(Clone)]
    struct DropCounter(Arc<StdMutex<usize>>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            *self.0.lock().unwrap() += 1;
        }
    }

    let outcomes: Arc<StdMutex<HashSet<Result<(), Outcome>>>> = Default::default();
    let outcomes_clone = outcomes.clone();

    check_dfs(
        move || {
            let drop_count = Arc::new(StdMutex::new(0usize));
            let dc = DropCounter(drop_count.clone());
            let t = future::spawn(async move {
                let _guard = dc;
                future::yield_now().await;
            });
            t.abort();
            let result = future::block_on(t);
            outcomes.lock().unwrap().insert(result.map_err(Outcome::from));
            // The DropCounter should have been dropped exactly once,
            // whether the task completed normally or was cancelled.
            assert_eq!(1, *drop_count.lock().unwrap());
        },
        None,
    );

    let seen = outcomes_clone.lock().unwrap();
    let expected: HashSet<Result<(), Outcome>> = [Ok(()), Err(Outcome::Cancelled)].into();
    assert_eq!(*seen, expected, "expected both outcomes, saw: {:?}", *seen);
}

/// When an aborted task's inner future is dropped, its destructor must still be
/// able to access thread-local storage. This would fail if TLS were destroyed
/// (via pop_local) before the inner future is dropped.
#[test]
fn abort_drop_can_access_thread_local() {
    use std::collections::HashSet;

    shuttle::thread_local! {
        static TLS_COUNTER: std::cell::Cell<usize> = std::cell::Cell::new(0);
    }

    struct AccessTlsOnDrop;

    impl Drop for AccessTlsOnDrop {
        fn drop(&mut self) {
            // This would panic with "cannot access a Thread Local Storage value
            // during or after destruction" if TLS were already destroyed.
            TLS_COUNTER.with(|c| c.set(c.get() + 1));
        }
    }

    let outcomes: Arc<std::sync::Mutex<HashSet<Result<(), Outcome>>>> = Default::default();
    let outcomes_clone = outcomes.clone();

    check_dfs(
        move || {
            let (tx, rx) = oneshot::channel();
            let t = future::spawn(async move {
                // Initialize the TLS slot and hold a guard that accesses it on drop.
                TLS_COUNTER.with(|c| c.set(42));
                let _guard = AccessTlsOnDrop;
                // Signal that TLS is initialized and the guard is held.
                tx.send(()).unwrap();
                future::yield_now().await;
            });
            // Wait until the task has initialized TLS and is holding the guard.
            future::block_on(async { rx.await.unwrap() });
            t.abort();
            let result = future::block_on(t);
            outcomes.lock().unwrap().insert(result.map_err(Outcome::from));
        },
        None,
    );

    let seen = outcomes_clone.lock().unwrap();
    // DFS must explore both: task cancelled and task completed before abort.
    let expected: HashSet<Result<(), Outcome>> = [Ok(()), Err(Outcome::Cancelled)].into();
    assert_eq!(*seen, expected, "expected both outcomes, saw: {:?}", *seen);
}

/// Aborting with AbortHandle while another task is awaiting the JoinHandle.
#[test]
fn abort_while_joined() {
    use std::collections::HashSet;

    let outcomes: Arc<std::sync::Mutex<HashSet<Result<u32, Outcome>>>> = Default::default();
    let outcomes_clone = outcomes.clone();

    check_dfs(
        move || {
            let t = future::spawn(async move {
                future::yield_now().await;
                42u32
            });
            let abort = t.abort_handle();
            // Spawn a task that awaits t's JoinHandle.
            let joiner = future::spawn(t);
            abort.abort();
            let result = future::block_on(joiner).unwrap();
            outcomes.lock().unwrap().insert(result.map_err(Outcome::from));
        },
        None,
    );

    let seen = outcomes_clone.lock().unwrap();
    let expected: HashSet<Result<u32, Outcome>> = [Ok(42u32), Err(Outcome::Cancelled)].into();
    assert_eq!(*seen, expected, "expected both outcomes, saw: {:?}", *seen);
}

/// AbortHandle clone works the same as the original.
#[test]
fn abort_handle_clone() {
    use std::collections::HashSet;

    let outcomes: Arc<std::sync::Mutex<HashSet<Result<(), Outcome>>>> = Default::default();
    let outcomes_clone = outcomes.clone();

    check_dfs(
        move || {
            let t = future::spawn(async move {
                future::yield_now().await;
            });
            let abort1 = t.abort_handle();
            let abort2 = abort1.clone();
            drop(abort1);
            abort2.abort();
            let result = future::block_on(t);
            outcomes.lock().unwrap().insert(result.map_err(Outcome::from));
        },
        None,
    );

    let seen = outcomes_clone.lock().unwrap();
    let expected: HashSet<Result<(), Outcome>> = [Ok(()), Err(Outcome::Cancelled)].into();
    assert_eq!(*seen, expected, "expected both outcomes, saw: {:?}", *seen);
}
