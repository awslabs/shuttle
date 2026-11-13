//! The same test bodies in both modes: without the `shuttle` feature they run on OS threads and
//! `std` types, with it they run as Shuttle tasks and Shuttle types.
//!
//! What each mode does differently is checked where it belongs: `shuttle-thread-impl`'s tests cover
//! the Shuttle behavior (the interleavings of spawned threads, the scheduling points of `sleep`,
//! `yield_now`, `park` and `spin_loop`, and the task- and execution-scoped `thread_local!` values).

use shuttle_thread::{hint, thread, thread_local};
use std::cell::Cell;
use std::sync::Arc;
use std::time::Duration;

thread_local! {
    static COUNT: Cell<usize> = Cell::new(0);
    static NAME: Cell<&'static str> = const { Cell::new("unnamed") };
}

/// Runs the body: under a Shuttle test with the feature, directly without it.
fn run(f: impl Fn() + Send + Sync + 'static) {
    #[cfg(feature = "shuttle")]
    shuttle::check_dfs(f, None);
    #[cfg(not(feature = "shuttle"))]
    f();
}

/// The macro declares a key of the `thread` module's own `LocalKey`, which is Shuttle's under the
/// feature and `std`'s without it. This only compiles if the two agree.
#[test]
fn the_macro_matches_the_module() {
    let _: &'static thread::LocalKey<Cell<usize>> = &COUNT;
    let _: &'static thread::LocalKey<Cell<&'static str>> = &NAME;
}

#[test]
fn a_thread_local_holds_a_value() {
    run(|| {
        COUNT.with(|count| {
            assert_eq!(count.get(), 0);
            count.set(1);
            assert_eq!(count.get(), 1);
        });
        NAME.with(|name| assert_eq!(name.get(), "unnamed"));
    });
}

#[test]
fn a_spawned_thread_runs_and_joins() {
    run(|| {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handle = {
            let counter = Arc::clone(&counter);
            thread::Builder::new()
                .name("worker".into())
                .spawn(move || {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    thread::current().id()
                })
                .unwrap()
        };
        assert_eq!(handle.thread().name(), Some("worker"));

        let id = handle.join().unwrap();
        assert_ne!(id, thread::current().id());
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
    });
}

#[test]
fn a_scope_lends_a_borrow() {
    run(|| {
        // Outside the scope, so that the threads inside it can borrow it.
        let data = [1usize, 2, 3];
        let total = thread::scope(|scope| {
            let halves = [
                scope.spawn(|| data[0]),
                thread::Builder::new()
                    .spawn_scoped(scope, || data[1] + data[2])
                    .unwrap(),
            ];
            halves.into_iter().map(|handle| handle.join().unwrap()).sum::<usize>()
        });
        assert_eq!(total, 6);
    });
}

#[test]
fn the_waiting_functions_return() {
    run(|| {
        // A zero sleep, because without the feature this really sleeps.
        thread::sleep(Duration::ZERO);
        thread::yield_now();
        hint::spin_loop();
        thread::current().unpark();
        thread::park();
        thread::current().unpark();
        thread::park_timeout(Duration::ZERO);
    });
}

#[test]
fn available_parallelism_has_a_value() {
    run(|| {
        let parallelism = thread::available_parallelism().unwrap().get();
        if cfg!(feature = "shuttle") {
            assert_eq!(parallelism, 4, "the configured value, not the host's");
        } else {
            assert!(parallelism > 0);
        }
    });
}
