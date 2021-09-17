use futures::future::poll_fn;
use shuttle::sync::atomic::{AtomicBool, Ordering};
use shuttle::sync::Mutex;
use shuttle::{asynch, check_dfs, thread};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use test_env_log::test;

#[test]
fn wake_after_finish() {
    #[derive(Clone)]
    struct Future1 {
        // We don't care about interleaving this lock; just using it to share the waker across tasks
        waker: std::sync::Arc<std::sync::Mutex<Option<Waker>>>,
    }

    impl Future1 {
        fn new() -> Self {
            Self {
                waker: std::sync::Arc::new(std::sync::Mutex::new(None)),
            }
        }
    }

    impl Future for Future1 {
        type Output = ();

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            *self.waker.lock().unwrap() = Some(cx.waker().clone());
            Poll::Ready(())
        }
    }

    check_dfs(
        || {
            let future1 = Future1::new();

            // Convert `future1` into an `async fn`, which is not allowed to be polled again after
            // returning `Ready`
            let future1_clone = future1.clone();
            asynch::block_on(async move {
                future1_clone.await;
            });

            let waker = future1.waker.lock().unwrap().take();
            if let Some(waker) = waker {
                waker.wake();
            }
        },
        None,
    )
}

// Test that we can pass wakers across threads and have them work correctly
#[test]
fn wake_during_poll() {
    check_dfs(
        || {
            let waker: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
            let waker_clone = Arc::clone(&waker);
            let signal = Arc::new(AtomicBool::new(false));
            let signal_clone = Arc::clone(&signal);

            // This thread might invoke `wake` before the other task finishes running a single
            // invocation of `poll`. If that happens, that task must not be blocked.
            thread::spawn(move || {
                signal_clone.store(true, Ordering::SeqCst);

                if let Some(waker) = waker_clone.lock().unwrap().take() {
                    waker.wake();
                }
            });

            asynch::block_on(poll_fn(move |cx| {
                *waker.lock().unwrap() = Some(cx.waker().clone());

                if signal.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }));
        },
        None,
    );
}

// Test that a waker invocation doesn't unblock a task that is blocked due to synchronization
// operations
#[test]
fn wake_during_blocked_poll() {
    static RAN_WAKER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    check_dfs(
        || {
            let waker: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
            let waker_clone = Arc::clone(&waker);
            let counter = Arc::new(Mutex::new(0));
            let counter_clone = Arc::clone(&counter);

            thread::spawn(move || {
                let mut counter = counter_clone.lock().unwrap();
                thread::yield_now();
                *counter += 1;
            });

            // If this `wake()` invocation happens while the thread above holds the `counter` lock
            // and the `block_on` task below is blocked waiting to acquire that same lock, then
            // `wake` must not unblock the `block_on` task. That is, `wake` should prevent the task
            // from being blocked *the next time it returns Pending*, not just any time it is
            // blocked.
            thread::spawn(move || {
                if let Some(waker) = waker_clone.lock().unwrap().take() {
                    RAN_WAKER.store(true, Ordering::SeqCst);
                    waker.wake();
                }
            });

            asynch::block_on(poll_fn(move |cx| {
                *waker.lock().unwrap() = Some(cx.waker().clone());

                let mut counter = counter.lock().unwrap();
                *counter += 1;

                Poll::Ready(())
            }));
        },
        None,
    );
    assert!(RAN_WAKER.load(Ordering::SeqCst), "waker was not invoked by any test");
}
