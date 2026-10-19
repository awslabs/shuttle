use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use shuttle::sync::{Mutex, RwLock};
use shuttle::{check_dfs, thread};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, PoisonError};
use test_log::test;

#[test]
fn mutex_poison() {
    check_dfs(
        || {
            let lock1 = Arc::new(Mutex::new(()));
            let lock1_clone = lock1.clone();

            let lock2 = Arc::new(Mutex::new(()));
            let _guard = lock2.lock();

            let jh = thread::spawn(move || {
                // panic while holding the mutex in order to poison it
                let _err = catch_unwind(move || {
                    let _lock = lock1_clone.lock().unwrap();
                    panic!();
                })
                .unwrap_err();
            });

            jh.join().unwrap();

            // lock1 should now be poisoned
            let result = lock1.lock();
            assert!(matches!(result, Err(PoisonError { .. })));

            drop(_guard);
            // lock2 should not be poisoned
            let lock_result = lock2.lock();
            assert!(lock_result.is_ok());
        },
        None,
    )
}

#[test]
fn rwlock_poison() {
    check_dfs(
        || {
            let lock1 = Arc::new(RwLock::new(()));
            let lock1_clone = lock1.clone();

            let lock2 = Arc::new(RwLock::new(()));
            let _guard = lock2.write();

            let jh = thread::spawn(move || {
                // panic while holding the mutex in order to poison it
                let _err = catch_unwind(move || {
                    let _lock = lock1_clone.write().unwrap();
                    panic!();
                })
                .unwrap_err();
            });

            jh.join().unwrap();

            // lock1 should now be poisoned
            let result = lock1.write();
            assert!(matches!(result, Err(PoisonError { .. })));

            drop(_guard);
            // lock2 should not be poisoned
            let lock_result = lock2.write();
            assert!(lock_result.is_ok());
        },
        None,
    )
}

/// Releases a semaphore's permit when dropped, as a lock guard does.
struct ReleaseOnDrop<'a>(&'a BatchSemaphore);

impl Drop for ReleaseOnDrop<'_> {
    fn drop(&mut self) {
        self.0.release(1);
    }
}

/// The main task holds a permit and panics, and releases the permit during unwinding. The panic is
/// caught and the execution continues, so the task that waits for the permit acquires it.
/// Previously, the release removed the waiting task from the waiter queue, and that task waited
/// forever.
fn check_waiter_gets_permit_when_holder_panics(fairness: Fairness) {
    check_dfs(
        move || {
            let semaphore = Arc::new(BatchSemaphore::new(1, fairness));
            semaphore.acquire_blocking(1).unwrap();
            let waiter = {
                let semaphore = Arc::clone(&semaphore);
                thread::spawn(move || semaphore.acquire_blocking(1).unwrap())
            };
            catch_unwind(AssertUnwindSafe(|| {
                let _guard = ReleaseOnDrop(&semaphore);
                thread::yield_now();
                panic!("expected");
            }))
            .unwrap_err();
            waiter.join().unwrap();
        },
        None,
    )
}

#[test]
fn fair_waiter_gets_permit_when_holder_panics() {
    check_waiter_gets_permit_when_holder_panics(Fairness::StrictlyFair);
}

#[test]
fn unfair_waiter_gets_permit_when_holder_panics() {
    check_waiter_gets_permit_when_holder_panics(Fairness::Unfair);
}

/// The main task locks a mutex while the holder task panics with the mutex locked. The main task
/// acquires the lock in every interleaving. The lock is poisoned or not, depending on the order.
/// Previously, the main task waited forever if it was waiting when the panic released the lock. It
/// panicked if the release ran between its poisoning check and its acquire.
#[test]
fn mutex_lock_while_holder_panics() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(0));
            let holder = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    catch_unwind(AssertUnwindSafe(|| {
                        let _guard = lock.lock().unwrap();
                        thread::yield_now();
                        panic!("expected");
                    }))
                    .unwrap_err();
                })
            };
            let mut guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
            *guard += 1;
            drop(guard);
            holder.join().unwrap();
            assert!(lock.lock().is_err());
        },
        None,
    )
}

/// The same test for an `RwLock`, with a reader task and a writer (the main task).
#[test]
fn rwlock_lock_while_holder_panics() {
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(0));
            let holder = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    catch_unwind(AssertUnwindSafe(|| {
                        let _guard = lock.write().unwrap();
                        thread::yield_now();
                        panic!("expected");
                    }))
                    .unwrap_err();
                })
            };
            let reader = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let _ = *lock.read().unwrap_or_else(PoisonError::into_inner);
                })
            };
            *lock.write().unwrap_or_else(PoisonError::into_inner) += 1;
            holder.join().unwrap();
            reader.join().unwrap();
            assert!(lock.write().is_err());
        },
        None,
    )
}

/// `into_inner` on a poisoned lock returns the data in a `PoisonError`, as in std. Previously, it
/// panicked: poisoning closed the semaphore of the lock, and `into_inner` acquires the semaphore.
#[test]
fn into_inner_of_poisoned_lock() {
    check_dfs(
        || {
            let mutex = Arc::new(Mutex::new(1));
            let rwlock = Arc::new(RwLock::new(2));
            let holder = {
                let (mutex, rwlock) = (Arc::clone(&mutex), Arc::clone(&rwlock));
                thread::spawn(move || {
                    catch_unwind(AssertUnwindSafe(|| {
                        let _mutex_guard = mutex.lock().unwrap();
                        let _rwlock_guard = rwlock.write().unwrap();
                        panic!("expected");
                    }))
                    .unwrap_err();
                })
            };
            holder.join().unwrap();
            let mutex = Arc::into_inner(mutex).unwrap();
            assert_eq!(mutex.into_inner().unwrap_err().into_inner(), 1);
            let rwlock = Arc::into_inner(rwlock).unwrap();
            assert_eq!(rwlock.into_inner().unwrap_err().into_inner(), 2);
        },
        None,
    )
}
