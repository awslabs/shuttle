use crate::runtime::execution::Execution;
use crate::runtime::task::{Task, TaskId};
use crate::runtime::thread::continuation::{ContinuationPool, CONTINUATION_POOL};
use crate::scheduler::metrics::MetricsScheduler;
use crate::scheduler::{Schedule, Scheduler};
use crate::sync::time::{
    constant_stepped::{
        ConstantSteppedTimeModel, ConstantTimeDistribution, Duration as ShuttleDuration, Instant as ShuttleInstant,
    },
    TimeModel,
};
use crate::Config;
use std::cell::RefCell;
use std::fmt;
use std::panic::{self, Location};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use tracing::{span, Level};

// A helper struct which on `drop` exits all current spans, then enters the span which was entered when it was constructed.
// The reason this exists is to solve the "span-stacking" issue which occurs when there is a panic inside `run` which is
// then caught by `panic::catch_unwind()` (such as when Shuttle is run inside proptest).
// In other words: it enables correct spans when doing proptest minimization.
#[must_use]
struct ResetSpanOnDrop {
    span: tracing::Span,
}

impl ResetSpanOnDrop {
    fn new() -> Self {
        Self {
            span: tracing::Span::current().clone(),
        }
    }
}

impl Drop for ResetSpanOnDrop {
    // Exits all current spans, then enters the span which was entered when `self` was constructed.
    fn drop(&mut self) {
        tracing::dispatcher::get_default(|subscriber| {
            while let Some(span_id) = tracing::Span::current().id().as_ref() {
                subscriber.exit(span_id);
            }
            if let Some(span_id) = self.span.id().as_ref() {
                subscriber.enter(span_id);
            }
        });
    }
}

/// A `Runner` is the entry-point for testing concurrent code.
///
/// It takes as input a function to test and a `Scheduler` to run it under. It then executes that
/// function as many times as dictated by the scheduler; each execution has its scheduling decisions
/// resolved by the scheduler, which can make different choices for each execution.
#[derive(Debug)]
pub struct Runner<S: ?Sized + Scheduler, T: ?Sized + TimeModel<ShuttleInstant, ShuttleDuration>> {
    scheduler: Rc<RefCell<MetricsScheduler<S>>>,
    time_model: Rc<RefCell<T>>,
    config: Config,
}

impl<S: Scheduler + 'static> Runner<S, ConstantSteppedTimeModel> {
    /// Construct a new `Runner` that will use the given `Scheduler` to control the test.
    pub fn new(scheduler: S, config: Config) -> Self {
        let metrics_scheduler = MetricsScheduler::new(scheduler);

        Self {
            scheduler: Rc::new(RefCell::new(metrics_scheduler)),
            time_model: Rc::new(RefCell::new(ConstantSteppedTimeModel::new(
                ConstantTimeDistribution::new(ShuttleDuration::from_micros(10)),
            ))),
            config,
        }
    }
}

impl<S: Scheduler + 'static, T: TimeModel<ShuttleInstant, ShuttleDuration> + 'static> Runner<S, T> {
    /// Construct a new `Runner` that will use the given `Scheduler` to control the test.
    pub fn new_with_time_model(scheduler: S, time_model: T, config: Config) -> Self {
        let metrics_scheduler = MetricsScheduler::new(scheduler);

        Self {
            scheduler: Rc::new(RefCell::new(metrics_scheduler)),
            time_model: Rc::new(RefCell::new(time_model)),
            config,
        }
    }

    /// Test the given function and return the number of times the function was invoked during the
    /// test (i.e., the number of iterations run).
    #[track_caller]
    pub fn run<F>(self, f: F) -> usize
    where
        F: Fn() + Send + Sync + 'static,
    {
        let _span_drop_guard = ResetSpanOnDrop::new();
        // Share continuations across executions to avoid reallocating them
        // TODO it would be a lot nicer if this were a more generic "context" thing that we passed
        // TODO around explicitly rather than being a thread local
        CONTINUATION_POOL.set(&ContinuationPool::new(), || {
            let f = Arc::new(f);

            let start = Instant::now();

            let mut i = 0;

            loop {
                if self.config.max_time.map(|t| start.elapsed() > t).unwrap_or(false) {
                    break;
                }

                let schedule = match self.scheduler.borrow_mut().new_execution() {
                    None => break,
                    Some(s) => s,
                };

                let execution = Execution::new(self.scheduler.clone(), schedule, self.time_model.clone());
                let f = Arc::clone(&f);

                // This is a slightly lazy way to ensure that everything outside of the "execution" span gets
                // established correctly between executions. Fully `exit`ing and fully `enter`ing (explicitly
                // `enter`/`exit` all `Span`s) would most likely obviate the need for this.
                let _span_drop_guard2 = ResetSpanOnDrop::new();

                span!(Level::ERROR, "execution", i)
                    .in_scope(|| execution.run(&self.config, move || f(), Location::caller()));

                i += 1;
            }
            i
        })
    }
}

/// A `PortfolioRunner` is the same as a `Runner`, except that it can run multiple different
/// schedulers (a "portfolio" of schedulers) in parallel. If any of the schedulers finds a failing
/// execution of the test, the entire run fails.
pub struct PortfolioRunner<T: TimeModel<ShuttleInstant, ShuttleDuration> + Clone + Send> {
    schedulers: Vec<Box<dyn Scheduler + Send + 'static>>,
    time_model: Box<T>,
    stop_on_first_failure: bool,
    config: Config,
}
impl PortfolioRunner<ConstantSteppedTimeModel> {
    /// Construct a new `PortfolioRunner` with no schedulers. If `stop_on_first_failure` is true,
    /// all schedulers will be terminated as soon as any fails; if false, they will keep running
    /// and potentially find multiple bugs.
    pub fn new(stop_on_first_failure: bool, config: Config) -> Self {
        Self {
            schedulers: Vec::new(),
            time_model: Box::new(ConstantSteppedTimeModel::new(ConstantTimeDistribution::new(
                ShuttleDuration::from_micros(10),
            ))),
            stop_on_first_failure,
            config,
        }
    }
}

impl<T: TimeModel<ShuttleInstant, ShuttleDuration> + Clone + Send + 'static> PortfolioRunner<T> {
    /// Construct a new `PortfolioRunner` with no schedulers. If `stop_on_first_failure` is true,
    /// all schedulers will be terminated as soon as any fails; if false, they will keep running
    /// and potentially find multiple bugs.
    pub fn new_with_time_model(stop_on_first_failure: bool, time_model: T, config: Config) -> Self {
        Self {
            schedulers: Vec::new(),
            time_model: Box::new(time_model),
            stop_on_first_failure,
            config,
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
        F: Fn() + Send + Sync + 'static,
    {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum ThreadResult {
            Passed,
            Failed,
        }

        let (tx, rx) = mpsc::sync_channel::<ThreadResult>(0);
        let stop_signal = Arc::new(AtomicBool::new(false));
        let config = self.config;
        let f = Arc::new(f);

        let threads = self
            .schedulers
            .into_iter()
            .enumerate()
            .map(|(i, scheduler)| {
                let f = Arc::clone(&f);
                let tx = tx.clone();
                let stop_signal = stop_signal.clone();
                let config = config.clone();

                let tm = (*self.time_model).clone();
                thread::spawn(move || {
                    let scheduler = PortfolioStoppableScheduler { scheduler, stop_signal };

                    let runner = Runner::new_with_time_model(scheduler, tm, config);

                    span!(Level::ERROR, "job", i).in_scope(|| {
                        let ret = panic::catch_unwind(panic::AssertUnwindSafe(|| runner.run(move || f())));

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
impl<T: TimeModel<ShuttleInstant, ShuttleDuration> + Clone + Send> fmt::Debug for PortfolioRunner<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("PortfolioRunner")
            .field("schedulers", &self.schedulers.len())
            .field("stop_on_first_failure", &self.stop_on_first_failure)
            .field("config", &self.config)
            .finish()
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

    fn next_task(
        &mut self,
        runnable_tasks: &[&Task],
        current_task: Option<TaskId>,
        is_yielding: bool,
    ) -> Option<TaskId> {
        if self.stop_signal.load(Ordering::SeqCst) {
            None
        } else {
            self.scheduler.next_task(runnable_tasks, current_task, is_yielding)
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.scheduler.next_u64()
    }
}
