pub mod batch_semaphore;

use crate::runtime::execution::ExecutionState;
use crate::runtime::thread;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Run a future to completion on the current thread.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let waker = ExecutionState::with(|state| state.current_mut().waker());
    let cx = &mut Context::from_waker(&waker);

    loop {
        let polled = {
            let _guard = crate::await_backtrace::PollGuard::new();
            future.as_mut().poll(cx)
        };
        match polled {
            Poll::Ready(result) => break result,
            Poll::Pending => {
                // The poll stack (and with it the await chain) is gone now; keep whatever the waker
                // clone recorded while it was still live.
                let await_site = crate::await_backtrace::take_captured();
                ExecutionState::with(|state| {
                    let task = state.current_mut();
                    task.backtrace = await_site;
                    task.sleep_unless_woken();
                });
                thread::switch();
            }
        }
    }
}

/// Yields execution back to the scheduler.
pub async fn yield_now() {
    struct YieldNow {
        yielded: bool,
    }

    impl Future for YieldNow {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.yielded {
                return Poll::Ready(());
            }

            self.yielded = true;
            cx.waker().wake_by_ref();
            ExecutionState::request_yield();
            Poll::Pending
        }
    }

    YieldNow { yielded: false }.await
}
