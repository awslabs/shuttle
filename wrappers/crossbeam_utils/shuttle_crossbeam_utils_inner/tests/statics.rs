//! A primitive that outlives the execution it was used in starts the next one over, so that one
//! execution does not depend on the ones before it.

use shuttle::check_random;
use shuttle_crossbeam_utils_inner::atomic::AtomicCell;
use shuttle_crossbeam_utils_inner::sync::{Parker, ShardedLock, Unparker, WaitGroup};
use shuttle_time_inner::time::Duration;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

// A `Parker` whose state carried over would start an execution with a token, or with the identifier
// of a task that does not exist in it. A `Parker` is `!Sync`, as crossbeam's is, so what the static
// keeps is an `Unparker`, which holds the same state.
#[test]
fn a_parker_in_a_real_static_starts_over() {
    static UNPARKER: OnceLock<Unparker> = OnceLock::new();
    check_random(
        || {
            let unparker = UNPARKER.get_or_init(|| Parker::new().unparker().clone());
            // A `Parker` on the state the static keeps.
            // Safety: the pointer is `Unparker::into_raw`'s, of a clone that nothing else holds.
            let parker = unsafe { Parker::from_raw(Unparker::into_raw(unparker.clone())) };
            // No token at the start of every execution, so this times out rather than returning
            // with a token that an earlier execution left.
            parker.park_timeout(Duration::from_millis(1));
            // One made here is gone again in the next execution.
            unparker.unpark();
        },
        10,
    );
}

// The lock and the wait group are built on Shuttle's own primitives, which reset with it.
#[test]
fn a_sharded_lock_and_a_wait_group_in_real_statics() {
    static LOCK: OnceLock<ShardedLock<u32>> = OnceLock::new();
    static GROUP: OnceLock<WaitGroup> = OnceLock::new();
    check_random(
        || {
            let lock = LOCK.get_or_init(|| ShardedLock::new(0));
            // A guard is never held across executions, so the lock is free at the start of each.
            *lock.write().unwrap() += 1;
            assert!(!lock.is_poisoned());
            // A clone of the group, dropped in this execution, leaves the count as it found it.
            let group = GROUP.get_or_init(WaitGroup::new);
            drop(group.clone());
        },
        10,
    );
}

// An `AtomicCell`'s value does carry over, as a `static` of one of Shuttle's own atomics does
// today; its clock does not, which is what would hide a race.
#[test]
fn an_atomic_cell_in_a_static_keeps_its_value_and_resets_its_clock() {
    static CELL: AtomicCell<u32> = AtomicCell::new(0);
    let executions = AtomicUsize::new(0);
    check_random(
        move || {
            let n = executions.fetch_add(1, Ordering::SeqCst) as u32;
            // What the previous execution stored, which is the documented difference.
            assert_eq!(CELL.load(), n);
            CELL.store(n + 1);
        },
        10,
    );
}

// The documented workaround: a cell behind Shuttle's `lazy_static!` is created afresh in every
// execution, so its value resets too.
shuttle::lazy_static! {
    static ref RESET: AtomicCell<u32> = AtomicCell::new(0);
}

#[test]
fn a_lazy_static_cell_resets_its_value() {
    check_random(
        || {
            assert_eq!(RESET.load(), 0);
            RESET.store(1);
        },
        10,
    );
}

/// Runs `test` on `tests` OS threads at once, as `cargo test` runs that many tests, each with its own
/// number, and returns the message of each one's panic, if it panicked.
fn at_the_same_time(tests: usize, test: impl Fn(usize) + Send + Sync + 'static) -> Vec<String> {
    let test = std::sync::Arc::new(test);
    let start = std::sync::Arc::new(std::sync::Barrier::new(tests));
    let threads = (0..tests)
        .map(|i| {
            let (test, start) = (test.clone(), start.clone());
            std::thread::spawn(move || {
                start.wait();
                test(i)
            })
        })
        .collect::<Vec<_>>();
    threads
        .into_iter()
        .filter_map(|thread| thread.join().err())
        .map(|panic| {
            panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default()
        })
        .collect()
}

// Tests that run at the same time and share a `static AtomicCell` can't race on its value: the cell
// keeps it to one execution at a time, as Shuttle's locks do (see `shuttle-engine`'s `ValueLock`),
// so a test whose operation finds another test's in the middle of the cell fails with the report of
// that, and no update is lost. Before, the cell assumed one execution at a time: its value, clock
// and generation were plain cells, and seven of eight such tests failed with "RefCell already
// borrowed", the others racing on the value.
#[test]
fn tests_running_at_the_same_time_never_race_on_a_static_atomic_cell() {
    const TESTS: usize = 8;
    const EXECUTIONS: usize = 300;
    const ADDS: usize = 20;
    static CELL: AtomicCell<u64> = AtomicCell::new(0);
    let failures = at_the_same_time(TESTS, |_| {
        check_random(
            || {
                for _ in 0..ADDS {
                    CELL.fetch_add(1);
                    let _ = CELL.load();
                }
            },
            EXECUTIONS,
        )
    });
    for failure in &failures {
        assert!(
            failure.contains("a guard from outside of the current execution holds its value"),
            "a test failed otherwise than with the cell's report: {failure}"
        );
    }
    if failures.is_empty() {
        // Every test's every add, none lost to another test's.
        check_random(|| assert_eq!(CELL.load(), (TESTS * EXECUTIONS * ADDS) as u64), 1);
    }
}

// The clock is each execution's own, also while the cell is used by a test that runs at the same
// time: here another test's operation comes between one task's store and another task's load, and
// the load still joins the storing task's clock. With one clock in the cell, which an operation of
// another execution started over, it would not.
#[test]
fn a_static_atomic_cell_keeps_each_executions_clock_its_own() {
    static CELL: AtomicCell<u32> = AtomicCell::new(0);
    let (stored, has_stored) = std::sync::mpsc::channel();
    let (used, was_used) = std::sync::mpsc::channel::<()>();
    let was_used = std::sync::Arc::new(std::sync::Mutex::new(was_used));
    let first = std::thread::spawn(move || {
        check_random(
            move || {
                CELL.store(0);
                let storer = {
                    let (stored, was_used) = (stored.clone(), was_used.clone());
                    shuttle::thread::spawn(move || {
                        CELL.store(1);
                        // The other test uses the cell before this execution goes on.
                        stored.send(()).unwrap();
                        was_used.lock().unwrap().recv().unwrap();
                    })
                };
                while CELL.load() != 1 {
                    shuttle::thread::yield_now();
                }
                // The load that saw the store joined the storing task's clock.
                assert!(
                    shuttle::current::clock().get(1) > 0,
                    "the load did not join the clock of the store it saw"
                );
                storer.join().unwrap();
            },
            1,
        )
    });
    has_stored.recv().unwrap();
    // Another test, between the store and the load.
    check_random(|| _ = CELL.load(), 1);
    used.send(()).unwrap();
    first.join().unwrap();
}

/// A value whose first comparison stops in the middle of the cell's operation until another test has
/// tried the cell, as a `PartialEq` that takes a while would.
#[derive(Clone, Copy, Debug)]
struct Stall(u32);

/// Where the first comparison of a `Stall` stops: once to let the other test try the cell, and once
/// to wait for it.
static STALL: std::sync::Barrier = std::sync::Barrier::new(2);
static STALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl PartialEq for Stall {
    fn eq(&self, other: &Self) -> bool {
        if !STALLED.swap(true, Ordering::SeqCst) {
            STALL.wait();
            STALL.wait();
        }
        self.0 == other.0
    }
}

impl Eq for Stall {}

// A test whose operation finds another test's in the middle of a `static` cell panics, naming that
// test's thread, rather than reach the value at the same time.
#[test]
fn a_static_atomic_cell_that_another_test_is_in_the_middle_of_is_reported() {
    static CELL: AtomicCell<Stall> = AtomicCell::new(Stall(0));
    let first = std::thread::Builder::new()
        .name("holder".into())
        .spawn(|| check_random(|| _ = CELL.compare_exchange(Stall(0), Stall(1)), 1))
        .unwrap();
    // The first test's compare-exchange compares, in the middle of the cell.
    STALL.wait();
    let result = std::thread::spawn(|| check_random(|| _ = CELL.load(), 1)).join();
    STALL.wait();
    first.join().unwrap();
    let panic = result.expect_err("the cell reports the other test's operation");
    let message = panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        message.contains("a guard of the execution on thread 'holder', which runs at the same time"),
        "another panic: {message}"
    );
}
