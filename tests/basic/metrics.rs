use shuttle::scheduler::RandomScheduler;
use shuttle::{Runner, check_random, thread};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Metadata, Subscriber};

// Simple `Subscriber` that just remembers the last value of the `iterations` field it has seen from
// a `MetricsScheduler`-generated event
#[derive(Clone)]
struct MetricsSubscriber {
    iterations: Arc<AtomicUsize>,
}

impl MetricsSubscriber {
    fn new() -> Self {
        Self {
            iterations: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Subscriber for MetricsSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        // We don't care about span equality so just use the same identity for everything
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        // If it's an event from the `MetricsScheduler` with an `iterations` counter, record it
        let metadata = event.metadata();
        if metadata.target() == "shuttle::scheduler::metrics" {
            struct FindIterationsVisitor(Option<u64>);
            impl Visit for FindIterationsVisitor {
                fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
                fn record_u64(&mut self, field: &Field, value: u64) {
                    if field.name() == "iterations" {
                        self.0 = Some(value);
                    }
                }
            }
            let mut visitor = FindIterationsVisitor(None);
            event.record(&mut visitor);
            if let Some(iterations) = visitor.0 {
                self.iterations.store(iterations as usize, Ordering::SeqCst);
            }
        }
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

// Note: `panic_iteration` is 1-indexed because "iterations" is a count
fn iterations_test(run_iterations: usize, panic_iteration: usize) {
    let metrics = MetricsSubscriber::new();
    let _guard = tracing::subscriber::set_default(metrics.clone());

    let iterations = Arc::new(AtomicUsize::new(0));

    let result = catch_unwind(AssertUnwindSafe(|| {
        check_random(
            move || {
                iterations.fetch_add(1, Ordering::SeqCst);
                if iterations.load(Ordering::SeqCst) >= panic_iteration {
                    panic!("expected panic");
                }

                thread::spawn(move || {
                    thread::yield_now();
                });
            },
            run_iterations,
        );
    }));

    assert_eq!(result.is_err(), panic_iteration <= run_iterations);
    assert_eq!(
        metrics.iterations.load(Ordering::SeqCst),
        run_iterations.min(panic_iteration)
    );
}

#[test]
fn iterations_test_basic() {
    iterations_test(10, 20);
}

#[test]
fn iterations_test_panic() {
    iterations_test(10, 1);
    iterations_test(10, 5);
    iterations_test(10, 10);
}

#[test]
fn iterations_without_running() {
    let metrics = MetricsSubscriber::new();

    {
        let _guard = tracing::subscriber::set_default(metrics.clone());
        let scheduler = RandomScheduler::new(10);
        let _runner = Runner::new(scheduler, Default::default());
    }

    assert_eq!(metrics.iterations.load(Ordering::SeqCst), 0);
}
