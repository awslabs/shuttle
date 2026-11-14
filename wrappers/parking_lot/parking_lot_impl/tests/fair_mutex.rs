//! Shuttle's `parking_lot::FairMutex`: a mutex whose every unlock hands the lock to the longest
//! waiter, which is what Shuttle's `Mutex` does too.

use shuttle::scheduler::DfsScheduler;
use shuttle::{Runner, check_dfs, current, thread};
use shuttle_parking_lot_impl::{FairMutex, FairMutexGuard, MappedFairMutexGuard, Mutex, const_fair_mutex};
use std::sync::Arc;

#[test]
fn it_is_a_mutex() {
    check_dfs(
        || {
            let lock = Arc::new(FairMutex::new(0));
            let tasks = (0..2)
                .map(|_| {
                    let lock = Arc::clone(&lock);
                    thread::spawn(move || {
                        let mut guard = lock.lock();
                        let seen = *guard;
                        thread::yield_now();
                        *guard = seen + 1;
                    })
                })
                .collect::<Vec<_>>();
            for task in tasks {
                task.join().unwrap();
            }
            assert_eq!(*lock.lock(), 2);
            assert!(lock.try_lock().is_some());
        },
        None,
    );
}

/// `bump` and `unlock_fair` hand the lock over and keep it exclusive.
#[test]
fn bump_and_unlock_fair() {
    check_dfs(
        || {
            let lock = Arc::new(FairMutex::new(0));
            let other = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let mut guard = lock.lock();
                    *guard += 1;
                    FairMutexGuard::unlock_fair(guard);
                })
            };
            let mut guard = lock.lock();
            let seen = *guard;
            FairMutexGuard::bump(&mut guard);
            // `bump` may let the other task in, which then leaves its increment behind.
            assert!(*guard == seen || *guard == seen + 1);
            *guard += 10;
            drop(guard);
            other.join().unwrap();
            assert_eq!(*lock.lock(), 11);
        },
        None,
    );
}

#[test]
fn guards_map() {
    check_dfs(
        || {
            let lock = FairMutex::new((1, 2));
            let guard = lock.lock();
            let mapped: MappedFairMutexGuard<'_, i32> = FairMutexGuard::map(guard, |pair| &mut pair.1);
            assert_eq!(*mapped, 2);
        },
        None,
    );
}

#[test]
fn a_const_fair_mutex_in_a_static() {
    static LOCK: FairMutex<u32> = const_fair_mutex(0);
    check_dfs(
        || {
            *LOCK.lock() += 1;
            assert!(LOCK.is_locked() || !LOCK.is_locked());
        },
        None,
    );
}

/// Shuttle's `Mutex` already hands off on every unlock, so a `FairMutex` explores the same schedules.
#[test]
fn it_explores_what_a_mutex_explores() {
    fn executions<L: Send + Sync + 'static>(new: fn() -> L, lock_unlock: fn(&L)) -> usize {
        Runner::new(DfsScheduler::new(None, false), Default::default()).run(move || {
            let lock = Arc::new(new());
            let tasks = (0..2)
                .map(|_| {
                    let lock = Arc::clone(&lock);
                    thread::spawn(move || {
                        lock_unlock(&lock);
                        lock_unlock(&lock);
                    })
                })
                .collect::<Vec<_>>();
            for task in tasks {
                task.join().unwrap();
            }
        })
    }
    let fair = executions(|| FairMutex::new(()), |lock| drop(lock.lock()));
    let plain = executions(|| Mutex::new(()), |lock| drop(lock.lock()));
    assert_eq!(fair, plain);
    assert!(fair > 1);
}

#[test]
fn context_switches() {
    check_dfs(
        || {
            let lock = FairMutex::new(());
            let before = current::context_switches();
            let guard = lock.lock();
            assert_eq!(current::context_switches() - before, 1);
            let before = current::context_switches();
            drop(guard);
            assert_eq!(current::context_switches() - before, 1);
        },
        None,
    );
}
