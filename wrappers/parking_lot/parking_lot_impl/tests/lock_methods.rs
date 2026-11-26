//! The lock methods beyond lock and unlock: `is_locked`, the timed locks, recursive reads.

use shuttle::{check_dfs, check_random, current, thread};
use shuttle_parking_lot_impl::{Mutex, RwLock, RwLockUpgradableReadGuard};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// `parking_lot`'s `is_locked` is a load: asking doesn't take the lock, so it can't make another
/// task's `try_lock` fail.
#[test]
fn is_locked_does_not_take_the_lock() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(()));
            let asker = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let _ = lock.is_locked();
                })
            };
            assert!(
                lock.try_lock().is_some(),
                "a try_lock failed while nobody held the lock"
            );
            asker.join().unwrap();
        },
        None,
    );
}

#[test]
fn rwlock_is_locked_does_not_take_the_lock() {
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(()));
            let asker = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let _ = lock.is_locked();
                    let _ = lock.is_locked_exclusive();
                })
            };
            assert!(
                lock.try_write().is_some(),
                "a try_write failed while nobody held the lock"
            );
            assert!(
                lock.try_read().is_some(),
                "a try_read failed while nobody held the lock"
            );
            asker.join().unwrap();
        },
        None,
    );
}

#[test]
fn is_locked_reports_the_lock_state() {
    check_dfs(
        || {
            let lock = RwLock::new(());
            assert!(!lock.is_locked() && !lock.is_locked_exclusive());
            let read = lock.read();
            assert!(lock.is_locked() && !lock.is_locked_exclusive());
            drop(read);
            let upgradable = lock.upgradable_read();
            assert!(lock.is_locked() && !lock.is_locked_exclusive());
            let write = RwLockUpgradableReadGuard::upgrade(upgradable);
            assert!(lock.is_locked() && lock.is_locked_exclusive());
            drop(write);
            assert!(!lock.is_locked());

            let mutex = Mutex::new(());
            let guard = mutex.lock();
            assert!(mutex.is_locked());
            drop(guard);
            assert!(!mutex.is_locked());
        },
        None,
    );
}

/// One scheduling point, as an atomic load has in Shuttle, so a task that waits for a lock by
/// checking it in a loop lets the holder run.
#[test]
fn is_locked_is_one_scheduling_point() {
    check_dfs(
        || {
            let mutex = Mutex::new(());
            let rwlock = RwLock::new(());
            let before = current::context_switches();
            let _ = mutex.is_locked();
            assert_eq!(current::context_switches() - before, 1);
            let before = current::context_switches();
            let _ = rwlock.is_locked();
            let _ = rwlock.is_locked_exclusive();
            assert_eq!(current::context_switches() - before, 2);
        },
        None,
    );
}

#[test]
fn a_spin_on_is_locked_lets_the_holder_run() {
    check_random(
        || {
            let lock = Arc::new(Mutex::new(()));
            let guard = lock.lock();
            let spinner = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || while lock.is_locked() {})
            };
            thread::yield_now();
            drop(guard);
            spinner.join().unwrap();
        },
        100,
    );
}

/// A timed lock never times out under Shuttle: it waits, however short the timeout, and gets the
/// lock.
#[test]
fn a_timed_lock_waits_for_the_lock() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(0));
            let rwlock = Arc::new(RwLock::new(0));
            let mut guard = lock.lock();
            let mut write = rwlock.write();
            let waiter = {
                let (lock, rwlock) = (Arc::clone(&lock), Arc::clone(&rwlock));
                thread::spawn(move || {
                    assert_eq!(*lock.try_lock_for(Duration::ZERO).expect("timed out"), 1);
                    assert_eq!(*lock.try_lock_until(Instant::now()).expect("timed out"), 1);
                    assert_eq!(*rwlock.try_read_for(Duration::ZERO).expect("timed out"), 1);
                    assert_eq!(*rwlock.try_write_until(Instant::now()).expect("timed out"), 1);
                    let upgradable = rwlock.try_upgradable_read_for(Duration::ZERO).expect("timed out");
                    let upgraded =
                        RwLockUpgradableReadGuard::try_upgrade_for(upgradable, Duration::ZERO).expect("timed out");
                    assert_eq!(*upgraded, 1);
                    drop(upgraded);
                    assert_eq!(*rwlock.try_read_recursive_for(Duration::ZERO).expect("timed out"), 1);
                })
            };
            *guard = 1;
            *write = 1;
            drop((guard, write));
            waiter.join().unwrap();
        },
        None,
    );
}

/// What a real run would end with a timeout, Shuttle reports as a deadlock.
#[test]
#[should_panic(expected = "deadlock")]
fn a_timed_lock_of_a_lock_nobody_releases_deadlocks() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(()));
            let guard = lock.lock();
            let waiter = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || lock.try_lock_for(Duration::from_millis(1)).is_some())
            };
            waiter.join().unwrap();
            drop(guard);
        },
        None,
    );
}

/// `parking_lot` grants a recursive read while a reader holds the lock, even if a writer waits, which
/// is what lets a reader read again without deadlocking behind the writer.
#[test]
fn a_recursive_read_passes_a_waiting_writer() {
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(0));
            let read = lock.read();
            let writer = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || *lock.write() += 1)
            };
            thread::yield_now();
            let again = lock.read_recursive();
            assert_eq!(*again, 0, "the writer got in while a reader held the lock");
            assert!(lock.try_read_recursive().is_some());
            drop((again, read));
            writer.join().unwrap();
            assert_eq!(*lock.read_recursive(), 1);
        },
        None,
    );
}

/// The positive control: a plain read queues behind the waiting writer, which waits for the first
/// read, as in `parking_lot`.
#[test]
#[should_panic(expected = "deadlock")]
fn a_plain_read_behind_a_waiting_writer_deadlocks() {
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(0));
            let read = lock.read();
            let writer = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || *lock.write() += 1)
            };
            thread::yield_now();
            let again = lock.read();
            drop((again, read));
            writer.join().unwrap();
        },
        None,
    );
}

/// A writer still excludes a recursive reader.
#[test]
fn a_recursive_read_waits_for_a_writer() {
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(0));
            let write = lock.write();
            let reader = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    if let Some(read) = lock.try_read_recursive() {
                        assert_eq!(*read, 1, "a recursive read got in while the writer held the lock");
                    }
                    assert_eq!(*lock.read_recursive(), 1);
                })
            };
            let mut write = write;
            *write = 1;
            drop(write);
            reader.join().unwrap();
        },
        None,
    );
}
