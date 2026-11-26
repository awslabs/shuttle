//! Shuttle's `parking_lot::ReentrantMutex`: `lock_api`'s, over Shuttle's raw mutex and task ids.

use shuttle::scheduler::RandomScheduler;
use shuttle::{Runner, check_dfs, check_random, check_uncontrolled_nondeterminism, current, thread};
use shuttle_parking_lot_impl::{MappedReentrantMutexGuard, ReentrantMutex, ReentrantMutexGuard};
use std::cell::RefCell;
use std::sync::Arc;

#[test]
fn a_task_locks_it_again() {
    check_dfs(
        || {
            let lock = ReentrantMutex::new(2);
            let a = lock.lock();
            let b = lock.lock();
            let c = lock.try_lock().expect("the owner's try_lock succeeds");
            assert_eq!((*a, *b, *c), (2, 2, 2));
            assert!(lock.is_owned_by_current_thread());
            drop((c, b));
            assert!(lock.is_locked());
            drop(a);
            assert!(!lock.is_locked());
            assert!(!lock.is_owned_by_current_thread());
        },
        None,
    );
}

/// Another task can't get in while one holds it, however many times.
#[test]
fn it_is_a_mutex() {
    check_dfs(
        || {
            let lock = Arc::new(ReentrantMutex::new(RefCell::new(0)));
            let other = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    assert!(!lock.is_owned_by_current_thread());
                    let outer = lock.lock();
                    let inner = lock.lock();
                    // A second `RefCell` borrow would panic if the two tasks were inside at once.
                    let mut value = inner.borrow_mut();
                    *value += 1;
                    thread::yield_now();
                    *value += 1;
                    drop(value);
                    drop((inner, outer));
                })
            };
            {
                let outer = lock.lock();
                let inner = lock.lock();
                let mut value = inner.borrow_mut();
                *value += 10;
                thread::yield_now();
                drop(value);
                drop((inner, outer));
            }
            other.join().unwrap();
            assert_eq!(*lock.lock().borrow(), 12);
        },
        None,
    );
}

#[test]
fn try_lock_fails_for_another_task() {
    check_dfs(
        || {
            let lock = Arc::new(ReentrantMutex::new(()));
            let guard = lock.lock();
            let other = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || lock.try_lock().is_none())
            };
            assert!(
                other.join().unwrap(),
                "another task's try_lock succeeded while the lock was held"
            );
            drop(guard);
        },
        None,
    );
}

/// Only the outermost lock and the last unlock touch the raw mutex, so only they are scheduling
/// points.
#[test]
fn only_the_outermost_lock_and_unlock_are_scheduling_points() {
    check_dfs(
        || {
            let lock = ReentrantMutex::new(());
            let count = |f: &dyn Fn()| {
                let before = current::context_switches();
                f();
                current::context_switches() - before
            };
            let outer = std::cell::Cell::new(None);
            assert_eq!(count(&|| outer.set(Some(lock.lock()))), 1, "outermost lock");
            let inner = std::cell::Cell::new(None);
            assert_eq!(count(&|| inner.set(Some(lock.lock()))), 0, "reentrant lock");
            assert_eq!(count(&|| drop(lock.try_lock())), 0, "reentrant try_lock and its unlock");
            assert_eq!(count(&|| drop(inner.take())), 0, "inner unlock");
            assert_eq!(count(&|| drop(outer.take())), 1, "last unlock");
        },
        None,
    );
}

#[test]
fn guards_map() {
    check_dfs(
        || {
            let lock = ReentrantMutex::new((1, 2));
            let guard: ReentrantMutexGuard<'_, (i32, i32)> = lock.lock();
            let mapped: MappedReentrantMutexGuard<'_, i32> = ReentrantMutexGuard::map(guard, |pair| &pair.1);
            assert_eq!(*mapped, 2);
            let again = lock.lock();
            assert_eq!(again.0, 1);
        },
        None,
    );
}

/// Every task, in every execution, gets its own id, so ownership never carries from one task to
/// another.
#[test]
fn every_task_is_its_own_owner() {
    check_random(
        || {
            let lock = Arc::new(ReentrantMutex::new(()));
            let tasks = (0..3)
                .map(|_| {
                    let lock = Arc::clone(&lock);
                    thread::spawn(move || {
                        let guard = lock.lock();
                        assert!(lock.is_owned_by_current_thread());
                        let nested = lock.lock();
                        thread::yield_now();
                        drop((nested, guard));
                        assert!(!lock.is_owned_by_current_thread());
                    })
                })
                .collect::<Vec<_>>();
            for task in tasks {
                task.join().unwrap();
            }
        },
        200,
    );
}

fn counter_log() -> Vec<usize> {
    let lock = Arc::new(ReentrantMutex::new(RefCell::new(Vec::new())));
    let tasks = (0..3)
        .map(|i| {
            let lock = Arc::clone(&lock);
            thread::spawn(move || {
                let guard = lock.lock();
                let again = lock.lock();
                again.borrow_mut().push(i);
                drop((again, guard));
            })
        })
        .collect::<Vec<_>>();
    for task in tasks {
        task.join().unwrap();
    }
    lock.lock().borrow().clone()
}

#[test]
fn replays_from_a_seed() {
    let run = |seed| {
        let logs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let logs_clone = Arc::clone(&logs);
        Runner::new(RandomScheduler::new_from_seed(seed, 50), Default::default())
            .run(move || logs_clone.lock().unwrap().push(counter_log()));
        Arc::try_unwrap(logs).unwrap().into_inner().unwrap()
    };
    let first = run(0x5EED);
    assert_eq!(first, run(0x5EED));
    assert_ne!(first, run(0xD1FF));
}

#[test]
fn uncontrolled_nondeterminism() {
    check_uncontrolled_nondeterminism(
        || {
            let _ = counter_log();
        },
        100,
    );
}

/// Reentrancy is per mutex: two tasks that take two reentrant mutexes in opposite orders deadlock.
#[test]
#[should_panic(expected = "deadlock")]
fn two_reentrant_mutexes_in_opposite_orders_deadlock() {
    check_dfs(
        || {
            let a = Arc::new(ReentrantMutex::new(()));
            let b = Arc::new(ReentrantMutex::new(()));
            let other = {
                let (a, b) = (Arc::clone(&a), Arc::clone(&b));
                thread::spawn(move || {
                    let _b = b.lock();
                    let _a = a.lock();
                })
            };
            let _a = a.lock();
            let _a_again = a.lock();
            let _b = b.lock();
            drop((_b, _a_again, _a));
            other.join().unwrap();
        },
        None,
    );
}
