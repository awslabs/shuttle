use proptest::proptest;
use proptest::test_runner::Config;
use shuttle::future::spawn;
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_random, thread};
use test_log::test;
use tracing::{instrument::Instrument, warn, warn_span};

// NOTE:
// All of the tests testing tracing will either have to have weak assertions,
// or be #[ignore]d. The reason for this is that they are not thread safe
// (since everything tracing is managed globally), and there is no way to
// ensure that the tests are run single-threaded.

// TODO: Custom Subscriber
// TODO: Test with record_steps_in_span enabled

fn tracing_nested_spans() {
    let lock = Arc::new(Mutex::new(0));
    let threads: Vec<_> = (0..3)
        .map(|i| {
            let lock = lock.clone();
            thread::spawn(move || {
                let outer = warn_span!("outer", tid = i);
                let _outer = outer.enter();
                {
                    let mut locked = lock.lock().unwrap();
                    let inner = warn_span!("inner", tid = i);
                    let _inner = inner.enter();
                    warn!("incrementing from {}", *locked);
                    *locked += 1;
                }
            })
        })
        .collect();

    for thread in threads {
        thread.join().unwrap();
    }
}

#[ignore]
#[test]
fn test_tracing_nested_spans() {
    check_random(tracing_nested_spans, 10);
}

fn tracing_nested_spans_panic_mod_5(number: usize) {
    let lock = Arc::new(Mutex::new(0));
    let threads: Vec<_> = (0..3)
        .map(|i| {
            let lock = lock.clone();
            thread::spawn(move || {
                let outer = warn_span!("outer", tid = i);
                let _outer = outer.enter();
                {
                    let mut locked = lock.lock().unwrap();
                    let inner = warn_span!("inner", tid = i);
                    let _inner = inner.enter();
                    warn!("incrementing from {}", *locked);
                    *locked += 1;
                }
                if number.is_multiple_of(5) {
                    panic!();
                }
            })
        })
        .collect();

    for thread in threads {
        thread.join().unwrap();
    }
}

// Test to check that spans don't stack on panic and that minimization works as it should
proptest! {
    #![proptest_config(
        Config { cases: 1000, failure_persistence: None, .. Config::default() }
    )]
    #[should_panic]
    #[ignore]
    #[test]
    fn test_stacks_cleaned_on_panic(i: usize) {
        check_random(move || {
            tracing_nested_spans_panic_mod_5(i);
        },
        10);
    }
}

async fn spawn_instrumented_futures() {
    let jhs = (0..2)
        .map(|_| {
            spawn(async {
                let span_id = tracing::Span::current().id();
                async {
                    thread::yield_now();
                }
                .instrument(warn_span!("Span"))
                .await;
                assert_eq!(span_id, tracing::Span::current().id())
            })
        })
        .collect::<Vec<_>>();
    for jh in jhs {
        jh.await.unwrap();
    }
}

#[ignore]
#[test]
fn instrumented_futures() {
    let outer_span = warn_span!("OUTER");
    let _e = outer_span.enter();
    let _res = tracing_subscriber::fmt::try_init();
    shuttle::check_random(
        || {
            shuttle::future::block_on(async move {
                spawn_instrumented_futures().await;
            })
        },
        1000,
    );
}

/// Holds a thread-scoped default subscriber on a helper OS thread while alive.
///
/// While any thread holds a scoped default, `tracing` routes `get_default` through its per-thread
/// state, and a `Span::current()` issued from *inside* a `get_default` callback on another thread
/// returns `Span::none()`. That is what a concurrently running test using
/// `tracing::subscriber::set_default` does to every other test in the process.
struct ScopedDefaultOnAnotherThread {
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ScopedDefaultOnAnotherThread {
    fn hold() -> Self {
        let (release, released) = std::sync::mpsc::channel::<()>();
        let (ready, is_ready) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let _guard = tracing::subscriber::set_default(tracing_subscriber::registry());
            ready.send(()).unwrap();
            let _ = released.recv();
        });
        is_ready.recv().unwrap();
        Self {
            release: Some(release),
            thread: Some(thread),
        }
    }
}

impl Drop for ScopedDefaultOnAnotherThread {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Counts how many entries Shuttle leaves on the test thread's entered-span stack, by entering a
/// marker span around a Shuttle run and counting the enters that are still unmatched after it.
#[derive(Default)]
struct EnteredDepth {
    depth: std::sync::atomic::AtomicIsize,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for EnteredDepth {
    fn on_enter(&self, _id: &tracing::span::Id, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        self.depth.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn on_exit(&self, _id: &tracing::span::Id, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        self.depth.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Regression test for an intermittent "tried to clone a span ... that already closed" panic in
/// `exit_task_span`, which surfaced in whichever test happened to be running when another test in
/// the same process held a thread-scoped default subscriber.
///
/// Shuttle's span bookkeeping used to call `Span::current()` from inside
/// `tracing::dispatcher::get_default`. While another thread holds a scoped default, that nested call
/// returns `Span::none()`, so the loop that exits the task's spans exited nothing, but the
/// unconditional `enter` of the execution span still ran. Each scheduling step then leaked one entry
/// on the thread's entered-span stack. Once those spans closed, a later `Span::current()` resolved
/// to one of them and panicked.
#[test]
fn span_bookkeeping_with_scoped_default_on_another_thread() {
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use tracing_subscriber::layer::SubscriberExt;

    let counter = Arc::new(EnteredDepth::default());
    let subscriber = tracing_subscriber::registry().with(ArcLayer(Arc::clone(&counter)));
    let _default = tracing::subscriber::set_default(subscriber);
    let _other = ScopedDefaultOnAnotherThread::hold();

    let spans_seen_across_a_switch = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&spans_seen_across_a_switch);

    // Enter a span around the whole run, so the test thread has a current span of its own when
    // Shuttle starts and finishes. `Runner::run` must restore exactly this state afterwards, which is
    // what exercises `ResetSpanOnDrop`.
    let around = warn_span!("around_runner");
    let around_entered = around.enter();

    let before = counter.depth.load(Ordering::Relaxed);
    check_random(
        move || {
            let span = warn_span!("user");
            let _entered = span.enter();
            let want = span.id();
            let t = thread::spawn(thread::yield_now);
            // Forces a context switch while `user` is entered, so Shuttle must save and restore it.
            thread::yield_now();
            seen.lock().unwrap().push(want == tracing::Span::current().id());
            t.join().unwrap();
        },
        50,
    );
    let after = counter.depth.load(Ordering::Relaxed);
    let caller_span_restored = tracing::Span::current().id() == around.id();
    drop(around_entered);

    assert_eq!(
        after - before,
        0,
        "Shuttle left {} span entries on the entered-span stack",
        after - before
    );
    assert!(
        caller_span_restored,
        "after the run, the caller's own span was no longer the current span"
    );
    let seen = spans_seen_across_a_switch.lock().unwrap();
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|restored| *restored),
        "a span entered before a context switch was not the current span after it"
    );
}

/// Lets a `Layer` behind an `Arc` be shared with the test body.
struct ArcLayer<L>(std::sync::Arc<L>);

impl<S: tracing::Subscriber, L: tracing_subscriber::Layer<S>> tracing_subscriber::Layer<S> for ArcLayer<L> {
    fn on_enter(&self, id: &tracing::span::Id, ctx: tracing_subscriber::layer::Context<'_, S>) {
        self.0.on_enter(id, ctx)
    }

    fn on_exit(&self, id: &tracing::span::Id, ctx: tracing_subscriber::layer::Context<'_, S>) {
        self.0.on_exit(id, ctx)
    }
}

/// An event's message, and the names of the spans it happened in, outermost first.
type RecordedEvent = (String, Vec<&'static str>);

/// Records every event it sees.
struct RecordEvents(std::sync::Arc<std::sync::Mutex<Vec<RecordedEvent>>>);

impl<S> tracing_subscriber::Layer<S> for RecordEvents
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        let scope = ctx
            .event_scope(event)
            .map(|scope| scope.from_root().map(|span| span.name()).collect())
            .unwrap_or_default();
        self.0.lock().unwrap().push((message.0, scope));
    }
}

/// Regression test for a task that yields inside `tracing::dispatcher::with_default`.
///
/// `tracing`'s default dispatcher is per OS thread, and every Shuttle task runs on the same one. So
/// the task's dispatcher used to stay installed while other tasks ran, and their events went to it
/// instead of the test's subscriber. Shuttle also looked for the yielding task's spans in that
/// dispatcher, found none, and left them entered in the test's subscriber, so later events were
/// attributed to the wrong task.
#[test]
fn default_dispatcher_is_per_task() {
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    const ITERATIONS: usize = 50;
    let events = Arc::new(Mutex::new(Vec::new()));
    let _default =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(RecordEvents(Arc::clone(&events))));
    let other = tracing::Dispatch::new(tracing_subscriber::registry());

    check_random(
        move || {
            let other = other.clone();
            let t = thread::spawn(move || {
                let span = warn_span!("task");
                let _entered = span.enter();
                tracing::dispatcher::with_default(&other, || {
                    for _ in 0..3 {
                        thread::yield_now();
                    }
                });
                warn!("task done");
            });
            for _ in 0..3 {
                warn!("main");
                thread::yield_now();
            }
            t.join().unwrap();
        },
        ITERATIONS,
    );

    // Stop recording and release the lock before asserting: a failing assertion panics, and Shuttle's
    // panic hook emits events of its own.
    drop(_default);
    let events = std::mem::take(&mut *events.lock().unwrap());
    let main: Vec<_> = events.iter().filter(|(message, _)| message == "main").collect();
    assert_eq!(
        main.len(),
        3 * ITERATIONS,
        "events from the main task went to the other task's dispatcher"
    );
    assert!(
        main.iter().all(|(_, scope)| !scope.contains(&"task")),
        "an event from the main task was attributed to the other task's span"
    );
    let task_done: Vec<_> = events.iter().filter(|(message, _)| message == "task done").collect();
    assert_eq!(task_done.len(), ITERATIONS);
    assert!(
        task_done.iter().all(|(_, scope)| scope.last() == Some(&"task")),
        "an event from the task was not inside the task's own span"
    );
}

/// Spawns a task that yields forever inside `with_default(other)`, and returns once it is inside.
fn spawn_task_yielding_inside_with_default(other: tracing::Dispatch) {
    use shuttle::sync::atomic::{AtomicBool, Ordering};

    let inside = Arc::new(AtomicBool::new(false));
    let task_inside = Arc::clone(&inside);
    thread::spawn(move || {
        tracing::dispatcher::with_default(&other, || {
            task_inside.store(true, Ordering::SeqCst);
            loop {
                thread::yield_now();
            }
        })
    });
    while !inside.load(Ordering::SeqCst) {
        thread::yield_now();
    }
}

/// When an execution is stopped, or panics, while a task is switched out inside `with_default`,
/// Shuttle leaks the task's stack instead of unwinding it, so the guard on that stack never
/// restores the outer default. The caller must still get its own default back.
#[test]
fn default_dispatcher_restored_when_execution_ends_early() {
    use shuttle::scheduler::RandomScheduler;
    use shuttle::{Config, MaxSteps, Runner};
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    let events = Arc::new(Mutex::new(Vec::new()));
    let _default =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(RecordEvents(Arc::clone(&events))));
    let other = tracing::Dispatch::new(tracing_subscriber::registry());

    let mut config = Config::new();
    config.max_steps = MaxSteps::ContinueAfter(100);
    let stopped_other = other.clone();
    Runner::new(RandomScheduler::new(3), config).run(move || {
        spawn_task_yielding_inside_with_default(stopped_other.clone());
        loop {
            thread::yield_now();
        }
    });
    warn!("after the stopped execution");

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        check_random(
            move || {
                spawn_task_yielding_inside_with_default(other.clone());
                panic!("expected panic");
            },
            1,
        )
    }));
    assert!(panicked.is_err());
    warn!("after the panicking execution");

    // Stop recording and release the lock before asserting (see `default_dispatcher_is_per_task`).
    drop(_default);
    let events = std::mem::take(&mut *events.lock().unwrap());
    for message in ["after the stopped execution", "after the panicking execution"] {
        assert!(
            events.iter().any(|(recorded, _)| recorded == message),
            "the caller's default was not reinstated: `{message}` went elsewhere"
        );
    }
}
