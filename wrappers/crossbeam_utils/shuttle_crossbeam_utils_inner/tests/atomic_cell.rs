//! `AtomicCell` under Shuttle: its operations are atomic steps, they carry causality, and a cell of
//! a type too large for an atomic instruction is no different.

mod common;

use common::{dfs_outcomes, random_outcomes};
use shuttle::sync::Arc;
use shuttle::{check_dfs, check_random, thread};
use shuttle_crossbeam_utils_inner::atomic::AtomicCell;

#[test]
fn concurrent_increments_lose_nothing() {
    check_dfs(
        || {
            let cell = Arc::new(AtomicCell::new(0u32));
            let threads = (0..3)
                .map(|_| {
                    let cell = Arc::clone(&cell);
                    thread::spawn(move || cell.fetch_add(1))
                })
                .collect::<Vec<_>>();
            let seen = threads.into_iter().map(|t| t.join().unwrap()).collect::<Vec<_>>();
            assert_eq!(cell.load(), 3);
            // Each increment saw a different value: the read-modify-write is one atomic step.
            let mut seen = seen;
            seen.sort_unstable();
            assert_eq!(seen, [0, 1, 2]);
        },
        None,
    );
}

// A cell that needs a lock in the real crate is atomic here too: a swap never sees a half-written
// value.
#[test]
fn a_large_value_is_written_whole() {
    check_dfs(
        || {
            let cell = Arc::new(AtomicCell::new([0u8; 32]));
            let writer = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || cell.store([7u8; 32]))
            };
            let seen = cell.load();
            assert!(seen == [0u8; 32] || seen == [7u8; 32], "{seen:?}");
            writer.join().unwrap();
            assert_eq!(cell.load(), [7u8; 32]);
        },
        None,
    );
}

// Both orders of two stores are explored, and the load sees one of the three states.
#[test]
fn the_scheduler_decides_who_stores_first() {
    let outcomes = dfs_outcomes(|| {
        let cell = Arc::new(AtomicCell::new(0u8));
        let threads = [1u8, 2]
            .map(|n| {
                let cell = Arc::clone(&cell);
                thread::spawn(move || cell.swap(n))
            })
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect::<Vec<_>>();
        (threads[0], threads[1], cell.load())
    });
    // Either thread can go first, which decides what each swap returned.
    assert!(outcomes.contains(&(0, 1, 2)), "{outcomes:?}");
    assert!(outcomes.contains(&(2, 0, 1)), "{outcomes:?}");
}

// A value stored in the cell carries the storing task's clock, so a task that loads the value sees
// what that task wrote before it: Shuttle reports no race on the data behind it.
#[test]
fn a_cell_carries_causality() {
    check_dfs(
        || {
            let data = Arc::new(AtomicCell::new(0u32));
            let flag = Arc::new(AtomicCell::new(false));
            let writer = {
                let (data, flag) = (Arc::clone(&data), Arc::clone(&flag));
                thread::spawn(move || {
                    data.store(42);
                    flag.store(true);
                })
            };
            if flag.load() {
                assert_eq!(data.load(), 42);
            }
            writer.join().unwrap();
        },
        None,
    );
}

#[test]
fn compare_exchange_lets_exactly_one_task_win() {
    check_dfs(
        || {
            let cell = Arc::new(AtomicCell::new(0u8));
            let winners = Arc::new(AtomicCell::new(0u8));
            let threads = (0..3)
                .map(|_| {
                    let (cell, winners) = (Arc::clone(&cell), Arc::clone(&winners));
                    thread::spawn(move || {
                        if cell.compare_exchange(0, 1).is_ok() {
                            winners.fetch_add(1);
                        }
                    })
                })
                .collect::<Vec<_>>();
            for thread in threads {
                thread.join().unwrap();
            }
            assert_eq!(winners.load(), 1);
            assert_eq!(cell.load(), 1);
        },
        None,
    );
}

// `fetch_update`'s function runs outside the atomic step, so it may use Shuttle's primitives, and
// it is called again when another task got in first.
#[test]
fn fetch_update_retries() {
    let calls = random_outcomes(
        || {
            let cell = Arc::new(AtomicCell::new(0u32));
            let other = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || cell.fetch_add(10))
            };
            let mut calls = 0;
            let result = cell.fetch_update(|value| {
                calls += 1;
                // A scheduling point inside the function, which the real crate also allows.
                thread::yield_now();
                Some(value + 1)
            });
            assert!(result.is_ok());
            other.join().unwrap();
            assert_eq!(cell.load(), 11);
            calls
        },
        100,
    );
    assert!(calls.contains(&1), "{calls:?}");
    assert!(
        calls.iter().any(|n| *n > 1),
        "the function is called again after a lost race: {calls:?}"
    );
}

#[test]
fn is_lock_free_matches_crossbeams_answer() {
    // The answer is the real crate's, so that code which branches on it behaves the same.
    assert_eq!(
        AtomicCell::<usize>::is_lock_free(),
        crossbeam_utils::atomic::AtomicCell::<usize>::is_lock_free()
    );
    assert_eq!(
        AtomicCell::<()>::is_lock_free(),
        crossbeam_utils::atomic::AtomicCell::<()>::is_lock_free()
    );
    assert_eq!(
        AtomicCell::<[u8; 1000]>::is_lock_free(),
        crossbeam_utils::atomic::AtomicCell::<[u8; 1000]>::is_lock_free()
    );
    assert_eq!(
        AtomicCell::<[u8; 3]>::is_lock_free(),
        crossbeam_utils::atomic::AtomicCell::<[u8; 3]>::is_lock_free()
    );
    assert_eq!(
        AtomicCell::<u64>::is_lock_free(),
        crossbeam_utils::atomic::AtomicCell::<u64>::is_lock_free()
    );
    assert_eq!(
        AtomicCell::<bool>::is_lock_free(),
        crossbeam_utils::atomic::AtomicCell::<bool>::is_lock_free()
    );
}

#[test]
fn a_cell_of_a_non_copy_type() {
    check_random(
        || {
            let cell = Arc::new(AtomicCell::new(String::from("first")));
            let writer = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || cell.swap(String::from("second")))
            };
            let taken = cell.take();
            let swapped = writer.join().unwrap();
            // Whichever went first took "first", and the other took what it left: the default that
            // `take` put there, or the swap's "second". No value is lost or duplicated.
            assert!(
                (taken == "first" && swapped.is_empty()) || (swapped == "first" && taken == "second"),
                "{taken:?} {swapped:?}"
            );
        },
        50,
    );
}

#[test]
#[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
fn an_operation_outside_of_a_test_panics() {
    let cell = AtomicCell::new(1u8);
    cell.store(2);
}

#[test]
fn the_constructors_work_outside_of_a_test() {
    static CELL: AtomicCell<u32> = AtomicCell::new(5);
    let cell = AtomicCell::new(1u8);
    assert_eq!(cell.into_inner(), 1);
    assert!(!CELL.as_ptr().is_null());
    assert!(AtomicCell::<u32>::is_lock_free());
}
