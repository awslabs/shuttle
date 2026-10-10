//! A global `tracing` subscriber that reads a task-local on every event, at TRACE level, so that
//! it also gets every event Shuttle emits about its own bookkeeping, including the one it emits for
//! every access to its execution state, which reading a task-local makes. That access must not
//! emit the event again from inside its handling, since the subscriber would then be called again
//! from inside the read, and so on until the stack overflows. (With a scoped subscriber, `tracing`
//! refuses the re-entrant call instead, so this needs a global one; a global subscriber is
//! process-wide, which is why this test has a binary of its own.)

use shuttle::future::block_on;
use shuttle_tokio_impl_inner::sync::Mutex;
use shuttle_tokio_impl_inner::{task, task_local};
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

task_local! {
    static KEY: u32;
}

static DEPTH: AtomicUsize = AtomicUsize::new(0);
static MAX_DEPTH: AtomicUsize = AtomicUsize::new(0);
static EVENTS: AtomicUsize = AtomicUsize::new(0);

struct ReadsKey;

impl<S: tracing::Subscriber> Layer<S> for ReadsKey {
    fn on_event(&self, _: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        EVENTS.fetch_add(1, Ordering::SeqCst);
        let depth = DEPTH.fetch_add(1, Ordering::SeqCst) + 1;
        MAX_DEPTH.fetch_max(depth, Ordering::SeqCst);
        // Stop at some depth rather than overflow the stack, so that the test fails instead of
        // crashing the process.
        if depth < 8 {
            let _ = KEY.try_get();
        }
        DEPTH.fetch_sub(1, Ordering::SeqCst);
    }
}

#[test]
fn reading_a_task_local_from_a_global_subscriber_does_not_recurse() {
    let subscriber =
        tracing_subscriber::registry().with(ReadsKey.with_filter(tracing_subscriber::filter::LevelFilter::TRACE));
    tracing::subscriber::set_global_default(subscriber).unwrap();

    shuttle::check_dfs(
        || {
            block_on(KEY.scope(1, async {
                let child = task::spawn(KEY.scope(2, async {
                    drop(Mutex::new(()).lock().await);
                    tracing::info!("in the child");
                }));
                drop(Mutex::new(()).lock().await);
                tracing::info!("in the parent");
                child.await.unwrap();
            }))
        },
        None,
    );

    assert!(EVENTS.load(Ordering::SeqCst) > 0, "the subscriber got no events");
    // Reading the task-local while handling any other event emits the event for the read's access
    // to the state, so the subscriber is called once more from in there, but the read it makes
    // then doesn't emit it.
    assert_eq!(
        MAX_DEPTH.load(Ordering::SeqCst),
        2,
        "reading a task-local emitted an event while that event was being handled"
    );
}
