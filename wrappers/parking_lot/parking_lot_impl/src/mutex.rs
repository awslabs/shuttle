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
    use super::Mutex;
    use shuttle::{check_dfs, thread};
    use std::sync::Arc;

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
