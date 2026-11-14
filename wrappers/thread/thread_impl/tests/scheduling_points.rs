//! Where the thread functions take a scheduling point, counted exactly.
//!
//! `current::context_switches` counts the scheduling points of the whole execution, so each test
//! below runs a single task: the only scheduling points are the ones the test takes itself.

use shuttle::scheduler::DfsScheduler;
use shuttle::{Config, Runner, check_dfs, current};
use shuttle_thread_impl::{hint, thread};
use std::num::NonZeroUsize;
use std::time::Duration;

/// Runs `f` as the only task of a single execution.
fn one_task(f: impl Fn() + Send + Sync + 'static) {
    check_dfs(f, None);
}

/// How many scheduling points `f` takes.
fn scheduling_points(f: impl FnOnce()) -> usize {
    let before = current::context_switches();
    f();
    current::context_switches() - before
}

#[test]
fn sleep_takes_one_scheduling_point() {
    one_task(|| {
        // Shuttle does not model time: the call returns at once, having let other tasks run once.
        assert_eq!(scheduling_points(|| thread::sleep(Duration::from_secs(60))), 1);
    });
}

#[test]
fn yield_now_takes_one_scheduling_point() {
    one_task(|| {
        assert_eq!(scheduling_points(thread::yield_now), 1);
    });
}

#[test]
fn spin_loop_takes_one_scheduling_point() {
    one_task(|| {
        // std's `spin_loop` is a hint to the processor and nothing else. Shuttle's also yields, so
        // that a busy-wait lets the task it waits for run.
        assert_eq!(scheduling_points(hint::spin_loop), 1);
    });
}

#[test]
fn an_available_park_token_takes_no_scheduling_point() {
    one_task(|| {
        // "Because the token is initially absent, `unpark` followed by `park` will result in the
        // second call returning immediately": the token is already there, so nothing blocks and
        // there is nothing for another task to observe.
        assert_eq!(scheduling_points(|| thread::current().unpark()), 1);
        assert_eq!(scheduling_points(thread::park), 0);
    });
}

#[test]
fn park_timeout_is_park() {
    one_task(|| {
        // The timeout never fires, so a `park_timeout` with a token available returns, and one
        // without a token blocks, exactly as `park` does. `park_blocks_until_unparked` below is the
        // blocking half.
        thread::current().unpark();
        assert_eq!(scheduling_points(|| thread::park_timeout(Duration::from_millis(1))), 0);
    });
}

/// A parked task blocks until another task unparks it, and `park_timeout` does the same: Shuttle
/// never fires the timeout.
#[test]
fn park_blocks_until_unparked() {
    for timeout in [false, true] {
        check_dfs(
            move || {
                let parked = thread::spawn(move || {
                    if timeout {
                        thread::park_timeout(Duration::from_millis(1));
                    } else {
                        thread::park();
                    }
                });
                parked.thread().unpark();
                parked.join().unwrap();
            },
            None,
        );
    }
}

#[test]
fn available_parallelism_is_the_configured_value() {
    one_task(|| {
        assert_eq!(thread::available_parallelism().unwrap().get(), 4, "the default");
        // Reading it is not a scheduling point: the value is configuration, not shared state.
        assert_eq!(
            scheduling_points(|| {
                let _ = thread::available_parallelism();
            }),
            0
        );
    });

    let mut config = Config::new();
    config.available_parallelism = NonZeroUsize::new(9).unwrap();
    let iterations = Runner::new(DfsScheduler::new(None, false), config).run(|| {
        assert_eq!(thread::available_parallelism().unwrap().get(), 9);
    });
    assert_eq!(iterations, 1);
}
