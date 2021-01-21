use crate::runtime::execution::Execution;
use crate::runtime::task::TaskId;
use crate::runtime::thread::continuation::{ContinuationPool, CONTINUATION_POOL};
use crate::scheduler::metrics::MetricsScheduler;
use crate::scheduler::{Schedule, Scheduler};
use std::cell::RefCell;
use std::panic;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use tracing::{span, Level};

/// A `Runner` is the entry-point for testing concurrent code.
///
/// It takes as input a function to test and a `Scheduler` to run it under. It then executes that
/// function as many times as dictated by the scheduler; each execution has its scheduling decisions
/// resolved by the scheduler, which can make different choices for each execution.
#[derive(Debug)]
pub struct Runner<S: ?Sized + Scheduler> {
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

            for i in 0.. {
                let schedule = match self.scheduler.borrow_mut().new_execution() {
                    None => break,
                    Some(s) => s,
                };
                let execution = Execution::new(self.scheduler.clone(), schedule);
                let f = Arc::clone(&f);
                span!(Level::INFO, "execution", i).in_scope(|| execution.run(move || f()));
            }
        })
    }
}

/// A `PortfolioRunner` is the same as a `Runner`, except that it can run multiple different
/// schedulers (a "portfolio" of schedulers) in parallel. If any of the schedulers finds a failing
/// execution of the test, the entire run fails.
#[derive(Debug)]
pub struct PortfolioRunner {
    schedulers: Vec<Box<dyn Scheduler + Send + 'static>>,
    stop_on_first_failure: bool,
}

impl PortfolioRunner {
    /// Construct a new `PortfolioRunner` with no schedulers. If `stop_on_first_failure` is true,
    /// all schedulers will be terminated as soon as any fails; if false, they will keep running
    /// and potentially find multiple bugs.
    pub fn new(stop_on_first_failure: bool) -> Self {
        Self {
            schedulers: Vec::new(),
            stop_on_first_failure,
        }
    }

    /// Add the given scheduler to the portfolio of schedulers to run the test with.
    pub fn add(&mut self, scheduler: impl Scheduler + Send + 'static) {
        self.schedulers.push(Box::new(scheduler));
    }

    /// Test the given function against all schedulers in parallel. If any of the schedulers finds
    /// a failing execution, this function panics.
    pub fn run<F>(self, f: F)
    where
        F: Fn() + Send + Sync + Clone + 'static,
    {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum ThreadResult {
            Passed,
            Failed,
        }

        let (tx, rx) = mpsc::sync_channel::<ThreadResult>(0);
        let stop_signal = Arc::new(AtomicBool::new(false));

        let threads = self
            .schedulers
            .into_iter()
            .enumerate()
            .map(|(i, scheduler)| {
                let f = f.clone();
                let tx = tx.clone();
                let stop_signal = stop_signal.clone();

                thread::spawn(move || {
                    let scheduler = PortfolioStoppableScheduler { scheduler, stop_signal };

                    let runner = Runner::new(scheduler);

                    span!(Level::INFO, "job", i).in_scope(|| {
                        let ret = panic::catch_unwind(panic::AssertUnwindSafe(|| runner.run(f)));

                        match ret {
                            Ok(_) => tx.send(ThreadResult::Passed),
                            Err(e) => {
                                tx.send(ThreadResult::Failed).unwrap();
                                panic::resume_unwind(e);
                            }
                        }
                    })
                })
            })
            .collect::<Vec<_>>();

        // Wait for each thread to pass or fail, and if any fails, tell all threads to stop early
        for _ in 0..threads.len() {
            if rx.recv().unwrap() == ThreadResult::Failed && self.stop_on_first_failure {
                stop_signal.store(true, Ordering::SeqCst);
            }
        }

        // Join all threads and propagate the first panic we see (note that this might not be the
        // same panic that caused us to stop, if multiple threads panic around the same time, but
        // that's probably OK).
        let mut panic = None;
        for thread in threads {
            if let Err(e) = thread.join() {
                panic = Some(e);
            }
        }
        assert!(stop_signal.load(Ordering::SeqCst) == panic.is_some());
        if let Some(e) = panic {
            std::panic::resume_unwind(e);
        }
    }
}

/// A wrapper around a `Scheduler` that can be told to stop early by setting a flag. We use this to
/// abort all jobs in a `PortfolioRunner` as soon as any job fails.
#[derive(Debug)]
struct PortfolioStoppableScheduler<S> {
    scheduler: S,
    stop_signal: Arc<AtomicBool>,
}

impl<S: Scheduler> Scheduler for PortfolioStoppableScheduler<S> {
    fn new_execution(&mut self) -> Option<Schedule> {
        if self.stop_signal.load(Ordering::SeqCst) {
            None
        } else {
            self.scheduler.new_execution()
        }
    }

    fn next_task(&mut self, runnable_tasks: &[TaskId], current_task: Option<TaskId>) -> Option<TaskId> {
        if self.stop_signal.load(Ordering::SeqCst) {
            None
        } else {
            self.scheduler.next_task(runnable_tasks, current_task)
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.scheduler.next_u64()
    }
}
