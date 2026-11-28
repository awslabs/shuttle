//! Tests of the model of `once_cell::sync::Lazy`.

use shuttle::scheduler::DfsScheduler;
use shuttle::{Runner, check_dfs, check_pct, check_uncontrolled_nondeterminism, current, thread};
use shuttle_once_cell_inner::sync::Lazy;
use std::collections::HashSet;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// once_cell's message, which differs from std's `LazyLock`'s.
const POISONED: &str = "Lazy instance has previously been poisoned";

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload.downcast::<&str>().map(|message| message.to_string()).unwrap(),
    }
}

#[test]
fn api() {
    check_dfs(
        || {
            let lazy = Lazy::new(|| 92);
            assert_eq!(Lazy::get(&lazy), None);
            assert_eq!(Lazy::force(&lazy), &92);
            assert_eq!(&*lazy, &92);
            assert_eq!(Lazy::get(&lazy), Some(&92));

            let mut lazy = lazy;
            assert_eq!(Lazy::get_mut(&mut lazy), Some(&mut 92));
            *lazy += 1;
            assert_eq!(Lazy::into_value(lazy).ok(), Some(93));

            let mut lazy = Lazy::new(|| 92);
            assert_eq!(Lazy::get_mut(&mut lazy), None);
            assert_eq!(Lazy::force_mut(&mut lazy), &mut 92);
            *lazy = 1;
            assert_eq!(*lazy, 1);

            // An uninitialized `Lazy` gives its initializer back. Nothing ties `T` to the
            // initializer here, in once_cell either, so it is named.
            let lazy: Lazy<i32, _> = Lazy::new(|| 92);
            let init = Lazy::into_value(lazy).unwrap_err();
            assert_eq!(init(), 92);

            // An initializer can capture, as long as it can be called once per execution.
            let base = String::from("base");
            let lazy = Lazy::new(move || format!("{base}+"));
            assert_eq!(lazy.as_str(), "base+");
        },
        None,
    );
}

#[test]
fn trait_impls() {
    check_dfs(
        || {
            let lazy = Lazy::<Vec<u8>>::default();
            assert_eq!(format!("{lazy:?}"), r#"Lazy { cell: OnceCell(Uninit), init: ".." }"#);
            assert!(lazy.is_empty());
            assert_eq!(format!("{lazy:?}"), r#"Lazy { cell: OnceCell([]), init: ".." }"#);
        },
        None,
    );
}

#[test]
fn context_switches() {
    check_dfs(
        || {
            let lazy = Lazy::new(|| 42);
            let start = current::context_switches();

            assert_eq!(Lazy::get(&lazy), None);
            assert_eq!(current::context_switches(), start);

            assert_eq!(*lazy, 42);
            assert_eq!(current::context_switches(), start + 2);

            assert_eq!(Lazy::force(&lazy), &42);
            assert_eq!(*lazy, 42);
            assert_eq!(current::context_switches(), start + 2);

            // A `&mut` borrow rules out a race, so forcing through one takes none.
            let mut lazy = Lazy::new(|| 1);
            assert_eq!(Lazy::force_mut(&mut lazy), &mut 1);
            *lazy += 1;
            assert_eq!(Lazy::into_value(lazy).ok(), Some(2));
            assert_eq!(current::context_switches(), start + 2);
        },
        None,
    );
}

// A `static` `Lazy` is uninitialized again in every execution, and its initializer runs once per
// execution, however many threads race to force it.
#[test]
fn static_resets_across_executions() {
    static INITIALIZERS: AtomicUsize = AtomicUsize::new(0);
    static LAZY: Lazy<usize> = Lazy::new(|| {
        INITIALIZERS.fetch_add(1, Ordering::SeqCst);
        42
    });

    let runner = Runner::new(DfsScheduler::new(None, false), Default::default());
    let executions = runner.run(|| {
        assert_eq!(Lazy::get(&LAZY), None);
        let threads = (0..2).map(|_| thread::spawn(|| *LAZY)).collect::<Vec<_>>();
        for thread in threads {
            assert_eq!(thread.join().unwrap(), 42);
        }
    });

    assert!(executions > 1);
    assert_eq!(INITIALIZERS.load(Ordering::SeqCst), executions);
}

// An initializer that panics poisons the `Lazy`: every later access panics with once_cell's
// message, `get` returns `None`, and `into_value` panics too.
#[test]
fn poison() {
    check_dfs(
        || {
            let lazy = Lazy::new(|| -> usize { panic!("expected panic") });
            let first = panic::catch_unwind(AssertUnwindSafe(|| *lazy)).unwrap_err();
            assert_eq!(panic_message(first), "expected panic");

            let second = panic::catch_unwind(AssertUnwindSafe(|| *lazy)).unwrap_err();
            assert_eq!(panic_message(second), POISONED);
            assert_eq!(Lazy::get(&lazy), None);

            let into_value = panic::catch_unwind(AssertUnwindSafe(|| Lazy::into_value(lazy).ok())).unwrap_err();
            assert_eq!(panic_message(into_value), POISONED);

            let mut lazy = Lazy::new(|| -> usize { panic!("expected panic") });
            assert!(panic::catch_unwind(AssertUnwindSafe(|| *Lazy::force_mut(&mut lazy))).is_err());
            let again = panic::catch_unwind(AssertUnwindSafe(|| *Lazy::force_mut(&mut lazy))).unwrap_err();
            assert_eq!(panic_message(again), POISONED);
            assert_eq!(Lazy::get_mut(&mut lazy), None);
        },
        None,
    );
}

// A thread that was waiting for an initializer that panicked panics with the poisoned message, as
// in once_cell, where it finds the initializer taken.
#[test]
fn poison_reaches_a_waiting_thread() {
    let messages = Arc::new(Mutex::new(HashSet::new()));
    let messages_clone = Arc::clone(&messages);
    check_dfs(
        move || {
            let lazy = Arc::new(Lazy::new(|| -> usize {
                thread::yield_now();
                panic!("expected panic")
            }));
            let threads = (0..2)
                .map(|_| {
                    let lazy = Arc::clone(&lazy);
                    thread::spawn(move || panic::catch_unwind(AssertUnwindSafe(|| **lazy)).unwrap_err())
                })
                .collect::<Vec<_>>();
            let mut seen = threads
                .into_iter()
                .map(|thread| panic_message(thread.join().unwrap()))
                .collect::<Vec<_>>();
            seen.sort();
            assert_eq!(
                seen,
                vec!["Lazy instance has previously been poisoned", "expected panic"]
            );
            messages_clone.lock().unwrap().insert(seen);
        },
        None,
    );
    assert_eq!(messages.lock().unwrap().len(), 1);
}

// The poisoning lasts for the execution in which the initializer panicked only.
#[test]
fn poison_resets_across_executions() {
    static EXECUTIONS: AtomicUsize = AtomicUsize::new(0);
    static LAZY: Lazy<usize> = Lazy::new(|| {
        if EXECUTIONS.load(Ordering::SeqCst).is_multiple_of(2) {
            panic!("expected panic");
        }
        42
    });

    let runner = Runner::new(shuttle::scheduler::RandomScheduler::new(10), Default::default());
    runner.run(|| {
        let execution = EXECUTIONS.fetch_add(1, Ordering::SeqCst) + 1;
        if execution.is_multiple_of(2) {
            assert!(panic::catch_unwind(|| *LAZY).is_err());
            let again = panic::catch_unwind(|| *LAZY).unwrap_err();
            assert_eq!(panic_message(again), POISONED);
        } else {
            assert_eq!(*LAZY, 42);
        }
    });
    assert_eq!(EXECUTIONS.load(Ordering::SeqCst), 10);
}

#[test]
fn racing_force() {
    fn race() {
        let initializers = Arc::new(AtomicUsize::new(0));
        let lazy = Arc::new(Lazy::new({
            let initializers = Arc::clone(&initializers);
            move || {
                initializers.fetch_add(1, Ordering::SeqCst);
                usize::from(thread::current().id())
            }
        }));
        let threads = (0..3)
            .map(|_| {
                let lazy = Arc::clone(&lazy);
                thread::spawn(move || **lazy)
            })
            .collect::<Vec<_>>();
        let values = threads.into_iter().map(|t| t.join().unwrap()).collect::<HashSet<_>>();
        assert_eq!(values.len(), 1);
        assert_eq!(initializers.load(Ordering::SeqCst), 1);
    }
    check_dfs(race, None);
    check_pct(race, 1_000, 3);
}

#[test]
#[should_panic(expected = "deadlock")]
fn reentrant_force() {
    static LAZY: Lazy<usize> = Lazy::new(|| *LAZY + 1);
    check_dfs(
        || {
            let _ = *LAZY;
        },
        None,
    );
}

#[test]
fn uncontrolled_nondeterminism() {
    static LAZY: Lazy<usize> = Lazy::new(|| usize::from(thread::current().id()));
    check_uncontrolled_nondeterminism(
        || {
            let threads = (0..3).map(|_| thread::spawn(|| *LAZY)).collect::<Vec<_>>();
            let values = threads.into_iter().map(|t| t.join().unwrap()).collect::<HashSet<_>>();
            assert_eq!(values.len(), 1);
        },
        100,
    );
}

// The same check fails for once_cell's own `Lazy`, which keeps its value across executions.
#[test]
#[should_panic(expected = "possible nondeterminism")]
fn uncontrolled_nondeterminism_with_once_cell_itself() {
    static LAZY: once_cell_orig::sync::Lazy<usize> = once_cell_orig::sync::Lazy::new(|| 1);
    check_uncontrolled_nondeterminism(
        || {
            if once_cell_orig::sync::Lazy::get(&LAZY).is_none() {
                thread::yield_now();
            }
            assert_eq!(*LAZY, 1);
        },
        10,
    );
}
