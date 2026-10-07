//! A global `tracing` subscriber that accesses Shuttle's execution state on every event, at TRACE
//! level, so that it also gets the event Shuttle emits for every such access. The subscriber's own
//! access must not emit that event again from inside its handling, since the subscriber would then
//! be called again from inside the access, and so on until the stack overflows. (With a scoped
//! subscriber, `tracing` refuses the re-entrant call instead, so this needs a global one; a global
//! subscriber is process-wide, which is why this test has a binary of its own.)

use shuttle::sync::Mutex;
use shuttle::{check_dfs, current, thread};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

static DEPTH: AtomicUsize = AtomicUsize::new(0);
static MAX_DEPTH: AtomicUsize = AtomicUsize::new(0);
static STATE_ACCESS_EVENTS: AtomicUsize = AtomicUsize::new(0);

/// Records whether an event is the one `ExecutionState::try_with` emits.
struct IsStateAccess(bool);

impl Visit for IsStateAccess {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}").starts_with("ExecutionState::try_with called from");
        }
    }
}

struct AccessesState;

impl<S: tracing::Subscriber> Layer<S> for AccessesState {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let depth = DEPTH.fetch_add(1, Ordering::SeqCst) + 1;
        MAX_DEPTH.fetch_max(depth, Ordering::SeqCst);
        let mut is_state_access = IsStateAccess(false);
        event.record(&mut is_state_access);
        if is_state_access.0 && depth == 1 {
            STATE_ACCESS_EVENTS.fetch_add(1, Ordering::SeqCst);
        }
        // Stop at some depth rather than overflow the stack, so that the test fails instead of
        // crashing the process.
        if depth < 8 {
            current::clock();
        }
        DEPTH.fetch_sub(1, Ordering::SeqCst);
    }
}

#[test]
fn accessing_the_state_from_a_global_subscriber_does_not_recurse() {
    let subscriber =
        tracing_subscriber::registry().with(AccessesState.with_filter(tracing_subscriber::filter::LevelFilter::TRACE));
    tracing::subscriber::set_global_default(subscriber).unwrap();

    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(0));
            let lock2 = lock.clone();
            let thread = thread::spawn(move || *lock2.lock().unwrap() += 1);
            *lock.lock().unwrap() += 1;
            thread.join().unwrap();
        },
        None,
    );

    // Shuttle's own accesses still emit the event.
    assert!(
        STATE_ACCESS_EVENTS.load(Ordering::SeqCst) > 0,
        "no access to the state emitted an event"
    );
    // The subscriber's `clock()` emits the event when the subscriber handles any other event, so the
    // subscriber is called once more from in there, but the `clock()` it makes then doesn't emit it.
    assert_eq!(
        MAX_DEPTH.load(Ordering::SeqCst),
        2,
        "an access to the state emitted an event while that event was being handled"
    );
}
