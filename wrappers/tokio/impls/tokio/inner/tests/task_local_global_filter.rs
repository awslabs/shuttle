//! A global `tracing` subscriber whose filter reads a task-local, at TRACE level. Reading a
//! task-local accesses Shuttle's state, and Shuttle emits an event for that access, which runs the
//! filter, which reads the task-local again. Shuttle has to set the flag that stops that before it
//! runs the filter, or this recurses until the stack overflows, inside a Shuttle test and outside of
//! one alike. (A global subscriber is process-wide, which is why this test has a binary of its own.)

use shuttle::check_dfs;
use shuttle_tokio_impl_inner::task_local;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

task_local! {
    static VERBOSE: bool;
}

static EVENTS: AtomicUsize = AtomicUsize::new(0);

struct CountsEvents;

impl<S: tracing::Subscriber> Layer<S> for CountsEvents {
    fn on_event(&self, _: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        EVENTS.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn filter_that_reads_a_task_local_does_not_recurse() {
    let filter = tracing_subscriber::filter::filter_fn(|_| VERBOSE.try_get().unwrap_or(true));
    let subscriber = tracing_subscriber::registry().with(CountsEvents.with_filter(filter));
    tracing::subscriber::set_global_default(subscriber).unwrap();

    VERBOSE.sync_scope(true, || tracing::info!("outside of a Shuttle test"));
    check_dfs(
        || VERBOSE.sync_scope(true, || tracing::info!("inside a Shuttle test")),
        None,
    );

    assert!(EVENTS.load(Ordering::SeqCst) > 0, "the subscriber got no events");
}
