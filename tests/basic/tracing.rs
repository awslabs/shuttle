use proptest::proptest;
use proptest::test_runner::Config;
use shuttle::future::spawn;
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_random, thread};
use std::time::Duration;
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
                if number % 5 == 0 {
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
                async {
                    let span_id = tracing::Span::current().id();
                    async {
                        // NOTE: Just a way to get a `thread::switch` call.
                        // Consider `pub`ing `thread::switch` ?
                        thread::sleep(Duration::from_millis(0));
                    }
                    .instrument(warn_span!("Span"))
                    .await;
                    assert_eq!(span_id, tracing::Span::current().id())
                }
                .await
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
        100,
    );
}
