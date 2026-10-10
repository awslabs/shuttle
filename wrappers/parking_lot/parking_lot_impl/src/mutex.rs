//! The user-facing `Mutex` types, mirroring [`parking_lot`]'s `mutex` module.
//!
//! These are the generic [`lock_api`] `Mutex` types specialised to Shuttle's
//! [`RawMutex`](crate::RawMutex). The `Arc`-based [`ArcMutexGuard`](lock_api::ArcMutexGuard)
//! is re-exported from the crate root (see `lib.rs`), matching `parking_lot`.
//!
//! [`parking_lot`]: <https://crates.io/crates/parking_lot>

use crate::raw_mutex::RawMutex;

/// A mutual exclusion primitive, backed by Shuttle. Mirrors `parking_lot::Mutex`.
pub type Mutex<T> = lock_api::Mutex<RawMutex, T>;

/// An RAII scoped lock guard for a [`Mutex`]. Mirrors `parking_lot::MutexGuard`.
pub type MutexGuard<'a, T> = lock_api::MutexGuard<'a, RawMutex, T>;

/// An RAII mutex guard returned by `MutexGuard::map`. Mirrors `parking_lot::MappedMutexGuard`.
pub type MappedMutexGuard<'a, T> = lock_api::MappedMutexGuard<'a, RawMutex, T>;

/// Creates a new mutex in an unlocked state ready for use.
///
/// This allows creating a mutex in a constant context on stable Rust. Mirrors
/// `parking_lot::const_mutex`.
pub const fn const_mutex<T>(val: T) -> Mutex<T> {
    Mutex::const_new(<RawMutex as lock_api::RawMutex>::INIT, val)
}

#[cfg(test)]
mod tests {
    use super::{Mutex, MutexGuard};
    use shuttle::{check_dfs, thread};
    use std::collections::HashSet;
    use std::sync::Arc;

    /// `is_locked` reads the lock state without taking the lock, as `parking_lot`'s does, so a
    /// concurrent `try_lock` cannot fail against it.
    #[test]
    fn is_locked_does_not_take_the_lock() {
        check_dfs(
            || {
                let lock = Arc::new(Mutex::new(()));
                let prober = {
                    let lock = Arc::clone(&lock);
                    thread::spawn(move || {
                        lock.is_locked();
                    })
                };
                assert!(
                    lock.try_lock().is_some(),
                    "try_lock failed on a mutex that no task holds"
                );
                prober.join().unwrap();
            },
            None,
        );
    }

    /// `is_locked` is true exactly while a task holds the lock.
    #[test]
    fn is_locked_reads_the_lock_state() {
        check_dfs(
            || {
                let lock = Arc::new(Mutex::new(()));
                assert!(!lock.is_locked());
                let guard = lock.lock();
                let prober = {
                    let lock = Arc::clone(&lock);
                    thread::spawn(move || assert!(lock.is_locked(), "is_locked is false while the lock is held"))
                };
                prober.join().unwrap();
                drop(guard);
                assert!(!lock.is_locked());
            },
            None,
        );
    }

    /// `parking_lot`'s `MutexGuard::bump` unlocks and locks again only while a task waits for the
    /// lock. A `try_lock` never waits, so it can't get in during the `bump`.
    #[test]
    fn bump_keeps_the_lock_while_no_task_waits() {
        check_dfs(
            || {
                let lock = Arc::new(Mutex::new(0));
                let other = {
                    let lock = Arc::clone(&lock);
                    thread::spawn(move || {
                        if let Some(mut g) = lock.try_lock() {
                            *g += 1;
                        }
                    })
                };
                let mut g = lock.lock();
                let before = *g;
                MutexGuard::bump(&mut g);
                assert_eq!(before, *g, "a try_lock got in during the bump");
                drop(g);
                other.join().unwrap();
            },
            None,
        );
    }

    /// A `bump` hands the lock to a task that waits for it, and the relock waits for that task. When
    /// the task has not asked yet, the `bump` keeps the lock.
    #[test]
    fn bump_yields_to_a_waiting_task() {
        let observed = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let observed_clone = Arc::clone(&observed);
        check_dfs(
            move || {
                let lock = Arc::new(Mutex::new(0));
                let mut g = lock.lock();
                let other = {
                    let lock = Arc::clone(&lock);
                    thread::spawn(move || *lock.lock() += 1)
                };
                thread::yield_now();
                MutexGuard::bump(&mut g);
                observed_clone.lock().unwrap().insert(*g);
                drop(g);
                other.join().unwrap();
            },
            None,
        );
        assert_eq!(
            *observed.lock().unwrap(),
            HashSet::from([0, 1]),
            "the bump should keep the lock, or yield it to the waiting task"
        );
    }

    #[test]
    fn smoke() {
        check_dfs(
            || {
                let m = Mutex::new(0);
                *m.lock() += 1;
                assert_eq!(*m.lock(), 1);
            },
            None,
        );
    }

    #[test]
    fn try_lock_contended() {
        check_dfs(
            || {
                let m = Arc::new(Mutex::new(()));
                let _g = m.lock();
                // Held by the current task, so a non-blocking try must fail.
                assert!(m.try_lock().is_none());
            },
            None,
        );
    }

    #[test]
    fn mutex_no_lost_updates() {
        check_dfs(
            || {
                let m = Arc::new(Mutex::new(0usize));
                let m2 = m.clone();
                let t = thread::spawn(move || {
                    *m2.lock() += 1;
                });
                *m.lock() += 1;
                t.join().unwrap();
                // Both increments must be observed; a broken `unlock` or missing exclusion would
                // let the two read-modify-write sequences interleave and drop one update.
                assert_eq!(*m.lock(), 2);
            },
            None,
        );
    }

    #[cfg(feature = "arc_lock")]
    #[test]
    fn arc_guard_mutual_exclusion() {
        check_dfs(
            || {
                let m = Arc::new(Mutex::new(0usize));
                let m2 = m.clone();
                let t = thread::spawn(move || {
                    // `lock_arc` returns an owned guard with no lifetime tied to `m2`.
                    let mut g = m2.lock_arc();
                    *g += 1;
                });
                {
                    let mut g = m.lock_arc();
                    *g += 1;
                }
                t.join().unwrap();
                assert_eq!(*m.lock(), 2);
            },
            None,
        );
    }
}
