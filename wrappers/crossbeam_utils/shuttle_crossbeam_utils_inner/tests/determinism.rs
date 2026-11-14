//! Replay: the primitives add no uncontrolled nondeterminism, so the same seed gives the same run
//! and `check_uncontrolled_nondeterminism` accepts them.

mod common;

use common::per_execution;
use shuttle::sync::Arc;
use shuttle::sync::atomic::{AtomicUsize, Ordering};
use shuttle::{check_uncontrolled_nondeterminism, thread};
use shuttle_crossbeam_utils_inner::atomic::AtomicCell;
use shuttle_crossbeam_utils_inner::sync::{Parker, ShardedLock, WaitGroup};
use shuttle_crossbeam_utils_inner::thread as cb_thread;
use shuttle_time_inner::time::Duration;

/// Who got to the cell and the lock first, which depends on the schedule.
fn race() -> Vec<u8> {
    let cell = Arc::new(AtomicCell::new(0u8));
    let lock = Arc::new(ShardedLock::new(Vec::new()));
    let threads = (1..=3u8)
        .map(|id| {
            let (cell, lock) = (Arc::clone(&cell), Arc::clone(&lock));
            thread::spawn(move || {
                if cell.compare_exchange(0, id).is_ok() {
                    lock.write().unwrap().push(id);
                }
                thread::yield_now();
                lock.write().unwrap().push(id + 10);
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    Arc::try_unwrap(lock).ok().unwrap().into_inner().unwrap()
}

#[test]
fn the_same_seed_gives_the_same_run() {
    assert_eq!(per_execution(race, 11, 20), per_execution(race, 11, 20));
}

#[test]
fn a_different_seed_gives_a_different_run() {
    assert_ne!(per_execution(race, 11, 20), per_execution(race, 12, 20));
}

#[test]
fn the_primitives_are_controlled_nondeterminism() {
    check_uncontrolled_nondeterminism(
        || {
            let counter = Arc::new(AtomicUsize::new(0));
            let cell = Arc::new(AtomicCell::new(0u8));
            let wg = WaitGroup::new();
            cb_thread::scope(|s| {
                for id in 1..=2u8 {
                    let (counter, cell, wg) = (Arc::clone(&counter), Arc::clone(&cell), wg.clone());
                    s.spawn(move |_| {
                        if cell.compare_exchange(0, id).is_ok() {
                            counter.fetch_add(1, Ordering::SeqCst);
                        }
                        drop(wg);
                    });
                }
                wg.wait();
            })
            .unwrap();
            // A branch on who won, so that an uncontrolled choice would change the schedule.
            if cell.load() == 1 {
                thread::spawn(|| {}).join().unwrap();
            }
            assert_eq!(counter.load(Ordering::SeqCst), 1);
        },
        100,
    );
}

#[test]
fn a_parker_and_a_backoff_replay() {
    check_uncontrolled_nondeterminism(
        || {
            let parker = Parker::new();
            let unparker = parker.unparker().clone();
            let ready = Arc::new(AtomicUsize::new(0));
            let worker = {
                let ready = Arc::clone(&ready);
                thread::spawn(move || {
                    ready.fetch_add(1, Ordering::SeqCst);
                    unparker.unpark();
                })
            };
            // A spin loop that waits for the worker, which needs the backoff's scheduling points.
            let backoff = shuttle_crossbeam_utils_inner::Backoff::new();
            while ready.load(Ordering::SeqCst) == 0 && !backoff.is_completed() {
                backoff.snooze();
            }
            parker.park_timeout(Duration::from_millis(1));
            worker.join().unwrap();
        },
        100,
    );
}

/// The real crate's primitives are outside Shuttle's control, which `thread.rs`'s
/// `the_real_crates_scope_is_not_a_shuttle_task` shows: its scoped thread is an OS thread, where
/// Shuttle's primitives panic. A real `AtomicCell` raced by real threads is uncontrolled too, but
/// whether a run of it differs from the next is up to the operating system, so there is no test of
/// it here that would not be flaky.
fn _why_there_is_no_test_of_the_real_atomic_cell() {}
