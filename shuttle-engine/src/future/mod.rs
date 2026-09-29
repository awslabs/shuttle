pub mod batch_semaphore;

use crate::runtime::execution::ExecutionState;
use crate::runtime::thread;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Run a future to completion on the current thread.
///
/// This is the `block_on` the synchronous primitives use on a task's behalf (see
/// [`BatchSemaphore::acquire_blocking`](batch_semaphore::BatchSemaphore::acquire_blocking)), so
/// unlike the driver loops for user futures it records no await site: a task parked here keeps its
/// whole call chain on its stack, and is captured lazily if the execution deadlocks. It can also run
/// inside a user future's `poll` (a `Mutex::lock` in an async fn), and must leave any await site that
/// poll has captured for the poll's own driver loop.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let waker = ExecutionState::with(|state| state.current_mut().waker());
    let cx = &mut Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(cx) {
            Poll::Ready(result) => break result,
            Poll::Pending => {
                ExecutionState::with(|state| state.current_mut().sleep_unless_woken());
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
