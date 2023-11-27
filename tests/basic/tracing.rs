use proptest::proptest;
use proptest::test_runner::Config;
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_random, thread};
use test_log::test;
use tracing::{warn, warn_span};

// TODO: Custom Subscriber
// TODO: Test with futures
// TODO: Test with panics
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

#[test]
fn test_tracing_nested_spans() {
    check_random(tracing_nested_spans, 10);
}

// Test to check that spans don't stack on panic and that minimization works as it should
proptest! {
    #![proptest_config(
        Config { cases: 1000, failure_persistence: None, .. Config::default() }
    )]
    #[should_panic]
    #[test]
    fn test_stacks_cleaned_on_panic(i: usize) {
        check_random(move || {
            tracing_nested_spans_panic_mod_5(i);
        },
        10);
    }
}
