//! Tests for `task::try_id`. (One test, as Shuttle's own events reach a scoped subscriber
//! unreliably while other tests in the process run Shuttle tests.)

use shuttle::check_dfs;
use shuttle::future::block_on;
use shuttle_tokio_impl_inner::task;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

/// Outside of a Shuttle test there is no task, as in tokio outside of a runtime. In a task, it is
/// the task's id. And a `tracing` subscriber can call it on every event, including Shuttle's own,
/// some of which Shuttle emits while it updates its own state, which a task can be doing.
#[test]
fn try_id() {
    assert_eq!(task::try_id(), None);

    #[derive(Default)]
    struct Seen {
        task: AtomicUsize,
        none: AtomicUsize,
    }

    struct CallsTryId(Arc<Seen>);

    impl<S: tracing::Subscriber> Layer<S> for CallsTryId {
        fn on_event(&self, _: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
            let seen = match task::try_id() {
                Some(_) => &self.0.task,
                None => &self.0.none,
            };
            seen.fetch_add(1, Ordering::SeqCst);
        }
    }

    let seen = Arc::new(Seen::default());
    let subscriber = tracing_subscriber::registry()
        .with(CallsTryId(seen.clone()).with_filter(tracing_subscriber::filter::LevelFilter::TRACE));
    tracing::subscriber::with_default(subscriber, || {
        check_dfs(
            || {
                let (try_id, id) = block_on(task::spawn(async { (task::try_id(), task::id()) })).unwrap();
                assert_eq!(try_id, Some(id));
            },
            None,
        );
    });

    assert!(seen.task.load(Ordering::SeqCst) > 0);
    assert!(seen.none.load(Ordering::SeqCst) > 0);
}
