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
