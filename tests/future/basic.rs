use futures::{try_join, Future};
use shuttle::sync::Mutex;
use shuttle::thread::park;
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
                    park();
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

            let wait_on_yield = future::spawn(async move {
                Timeout {
                    inner: Box::pin(yielder),
                    counter: 2,
                }
                .await
            });

            drop(wait_on_yield);
        },
        None,
    );
}
