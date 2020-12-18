use crate::runtime::execution::Execution;
use crate::runtime::thread::continuation::{ContinuationPool, CONTINUATION_POOL};
use crate::scheduler::metrics::MetricsScheduler;
use crate::scheduler::Scheduler;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use tracing::{span, Level};

/// A `Runner` is the entry-point for testing concurrent code.
///
/// It takes as input a function to test and a `Scheduler` to run it under. It then executes that
/// function as many times as dictated by the scheduler; each execution has its scheduling decisions
/// resolved by the scheduler, which can make different choices for each execution.
#[derive(Debug)]
pub struct Runner<S> {
    scheduler: Rc<RefCell<MetricsScheduler<S>>>,
}

impl<S: Scheduler + 'static> Runner<S> {
    /// Construct a new `Runner` that will use the given `Scheduler` to control the test.
    pub fn new(scheduler: S) -> Self {
        let metrics_scheduler = MetricsScheduler::new(scheduler);

        Self {
            scheduler: Rc::new(RefCell::new(metrics_scheduler)),
        }
    }

    /// Test the given function.
    pub fn run<F>(self, f: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        // Share continuations across executions to avoid reallocating them
        // TODO it would be a lot nicer if this were a more generic "context" thing that we passed
        // TODO around explicitly rather than being a thread local
        CONTINUATION_POOL.set(&ContinuationPool::new(), || {
            let f = Arc::new(f);

            let mut i = 0;
            while self.scheduler.borrow_mut().new_execution() {
                let execution = Execution::new(self.scheduler.clone());
                let f = Arc::clone(&f);
                span!(Level::DEBUG, "execution", i).in_scope(|| execution.run(move || f()));
                i += 1;
            }
        })
    }
}
