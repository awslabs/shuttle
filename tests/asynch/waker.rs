use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use shuttle::{asynch, check_dfs};
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
