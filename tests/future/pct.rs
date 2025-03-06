use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use shuttle::scheduler::PctScheduler;
use shuttle::sync::Arc;
use shuttle::{Config, MaxSteps, Runner, future};

/// Like [`shuttle::future::yield_now`] but doesn't request a yield from the scheduler
struct UnfairYieldNow {
    yielded: bool,
}

impl Future for UnfairYieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            return Poll::Ready(());
        }

        self.yielded = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

// Check that PCT correctly deprioritizes a yielding task. If it wasn't, there would be some
// iteration of this test where the yielding task has the highest priority and so the others
// never make progress.
fn yield_spin_loop(use_yield: bool) {
    const NUM_TASKS: usize = 4;

    let scheduler = PctScheduler::new(1, 100);
    let mut config = Config::new();
    config.max_steps = MaxSteps::FailAfter(50);
    let runner = Runner::new(scheduler, config);
    runner.run(move || {
        let count = Arc::new(AtomicUsize::new(0usize));

        let _thds = (0..NUM_TASKS)
            .map(|_| {
                let count = count.clone();
                future::spawn(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect::<Vec<_>>();

        future::block_on(async move {
            while count.load(Ordering::SeqCst) < NUM_TASKS {
                if use_yield {
                    future::yield_now().await;
                } else {
                    let yielder = UnfairYieldNow { yielded: false };
                    yielder.await;
                }
            }
        })
    });
}

#[test]
fn yield_spin_loop_fair() {
    yield_spin_loop(true);
}

#[test]
#[should_panic(expected = "exceeded max_steps bound")]
fn yield_spin_loop_unfair() {
    yield_spin_loop(false);
}
