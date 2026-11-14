//! A primitive in a `static` outlives the Shuttle execution that used it, and two Shuttle tests
//! running at the same time on different threads share it. Its Shuttle state belongs to the
//! running execution, so every execution starts with it unlocked.
//! The data a lock protects is not part of that state: tests running at the same time take turns
//! on it, and a guard that outlives its execution still holds it, so the next execution that takes
//! the lock reports that guard rather than hand out a second reference to the data
//! (`shuttle_engine::runtime::per_execution::ValueLock`).

use shuttle::check_random;
use shuttle_parking_lot_impl::{Mutex, RwLock, const_mutex, const_rwlock};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The message of the panic that reports a guard that an ended execution left holding a lock's
/// data (see `ValueLock`).
const LEAKED_GUARD: &str = "a guard left behind by an execution that has ended";

/// Runs `run`, which must fail in its second execution, where it takes a lock whose guard the first
/// execution forgot, with the report of that guard.
fn the_second_execution_reports_the_forgotten_guard(run: impl FnOnce()) {
    let panic = catch_unwind(AssertUnwindSafe(run)).expect_err("the second execution reports the forgotten guard");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or_default();
    assert!(message.contains(LEAKED_GUARD), "another panic: {message}");
}

/// An execution that ends with a `static` lock held (a forgotten guard here; a failed execution
/// leaks the guards of its tasks the same way) leaves the next execution an unlocked lock. The
/// forgotten guard still holds the lock's data, though, so the next execution's `lock` reports it,
/// once `is_locked` has shown that the lock itself is free.
#[test]
fn a_static_mutex_starts_every_execution_unlocked() {
    static LOCK: Mutex<()> = const_mutex(());
    the_second_execution_reports_the_forgotten_guard(|| {
        check_random(
            || {
                assert!(!LOCK.is_locked());
                std::mem::forget(LOCK.lock());
                assert!(LOCK.is_locked());
            },
            10,
        )
    });
}

#[test]
fn a_static_rwlock_starts_every_execution_unlocked() {
    static WRITTEN: RwLock<()> = const_rwlock(());
    the_second_execution_reports_the_forgotten_guard(|| {
        check_random(
            || {
                assert!(!WRITTEN.is_locked());
                std::mem::forget(WRITTEN.write());
                assert!(WRITTEN.is_locked_exclusive());
            },
            10,
        )
    });
    // A lock of its own: `WRITTEN`'s forgotten guard would be reported here too.
    static READ: RwLock<()> = const_rwlock(());
    the_second_execution_reports_the_forgotten_guard(|| {
        check_random(
            || {
                assert!(!READ.is_locked());
                std::mem::forget(READ.upgradable_read());
                std::mem::forget(READ.read());
            },
            10,
        )
    });
}

/// The protected data is the caller's, kept inside the lock, so it carries over. A test that needs
/// it to start afresh puts the lock in a `shuttle::lazy_static!`.
#[test]
fn the_data_of_a_static_lock_carries_over() {
    static COUNT: Mutex<usize> = const_mutex(0);
    check_random(|| *COUNT.lock() += 1, 10);
    assert_eq!(*data_between_runs(&COUNT), 10);
}

/// A lock's data, between Shuttle runs, where the lock can't be taken (it is a Shuttle primitive).
#[allow(clippy::mut_from_ref)]
fn data_between_runs<T>(lock: &Mutex<T>) -> &mut T {
    // Safety: called only between Shuttle runs, when no task holds or uses the lock.
    unsafe { &mut *lock.data_ptr() }
}
