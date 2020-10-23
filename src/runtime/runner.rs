use crate::runtime::execution::Execution;
use crate::scheduler::Scheduler;
use std::sync::Arc;
use tracing::{span, Level};

/// A `Runner` is the entry-point for testing concurrent code.
///
/// It takes as input a function to test and a `Scheduler` to run it under. It then executes that
/// function as many times as dictated by the scheduler; each execution has its scheduling decisions
/// resolved by the scheduler, which can make different choices for each execution.
#[derive(Debug)]
pub struct Runner {
    scheduler: Box<dyn Scheduler>,
}

impl Runner {
    /// Construct a new `Runner` that will use the given `Scheduler` to control the test.
    pub fn new(scheduler: impl Scheduler + 'static) -> Self {
        Self {
            scheduler: Box::new(scheduler),
        }
    }

    /// Test the given function.
    pub fn run<F>(mut self, f: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        let f = Arc::new(f);

        let mut i = 0;
        while self.scheduler.new_execution() {
            let execution = Execution::new(&mut self.scheduler);
            let f = Arc::clone(&f);
            span!(Level::DEBUG, "execution", i).in_scope(|| execution.run(move || f()));
            i += 1;
        }
    }
}
