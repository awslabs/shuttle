//! The user-facing `RwLock` types, mirroring [`parking_lot`]'s `rwlock` module.
//!
//! These are the generic [`lock_api`] `RwLock` types specialised to Shuttle's
//! [`RawRwLock`](crate::RawRwLock). The `Arc`-based guards
//! ([`ArcRwLockReadGuard`](lock_api::ArcRwLockReadGuard) etc.) are re-exported from the crate root
//! (see `lib.rs`), matching `parking_lot`.
//!
//! [`parking_lot`]: <https://crates.io/crates/parking_lot>

use crate::raw_rwlock::RawRwLock;

/// A reader-writer lock, backed by Shuttle. Mirrors `parking_lot::RwLock`.
pub type RwLock<T> = lock_api::RwLock<RawRwLock, T>;

/// RAII structure used to release shared read access when dropped. Mirrors
/// `parking_lot::RwLockReadGuard`.
pub type RwLockReadGuard<'a, T> = lock_api::RwLockReadGuard<'a, RawRwLock, T>;

/// RAII structure used to release exclusive write access when dropped. Mirrors
/// `parking_lot::RwLockWriteGuard`.
pub type RwLockWriteGuard<'a, T> = lock_api::RwLockWriteGuard<'a, RawRwLock, T>;

/// RAII read lock guard returned by `RwLockReadGuard::map`. Mirrors
/// `parking_lot::MappedRwLockReadGuard`.
pub type MappedRwLockReadGuard<'a, T> = lock_api::MappedRwLockReadGuard<'a, RawRwLock, T>;

/// RAII write lock guard returned by `RwLockWriteGuard::map`. Mirrors
/// `parking_lot::MappedRwLockWriteGuard`.
pub type MappedRwLockWriteGuard<'a, T> = lock_api::MappedRwLockWriteGuard<'a, RawRwLock, T>;

/// RAII structure used to release upgradable read access when dropped. Mirrors
/// `parking_lot::RwLockUpgradableReadGuard`.
pub type RwLockUpgradableReadGuard<'a, T> = lock_api::RwLockUpgradableReadGuard<'a, RawRwLock, T>;

/// Creates a new instance of an `RwLock` which is unlocked.
///
/// This allows creating an `RwLock` in a constant context on stable Rust. Mirrors
/// `parking_lot::const_rwlock`.
pub const fn const_rwlock<T>(val: T) -> RwLock<T> {
    RwLock::const_new(<RawRwLock as lock_api::RawRwLock>::INIT, val)
}

#[cfg(test)]
mod tests {
    use super::{RwLock, RwLockUpgradableReadGuard, RwLockWriteGuard};
    use shuttle::{
        check_dfs,
        thread::{self, spawn},
    };
    use std::sync::{Arc, atomic::Ordering};

    #[test]
    #[should_panic = "deadlock"]
    fn mem_forget_write_guard_deadlock() {
        check_dfs(
            move || {
                let rwlock = Arc::new(RwLock::new(()));
                let r1 = rwlock.clone();
                let t1 = spawn(move || {
                    std::mem::forget(r1.write());
                });
                let t2 = spawn(move || {
                    let _g = rwlock.write();
                });
                t1.join().unwrap();
                t2.join().unwrap();
            },
            None,
        );
    }

    // Same as above, but checking that we don't allow multiple upgradable read locks at the same time.
    #[test]
    #[should_panic = "deadlock"]
    fn mem_forget_upgradable_read_guard_deadlock() {
        check_dfs(
            move || {
                let rwlock = Arc::new(RwLock::new(()));
                let r1 = rwlock.clone();
                let t1 = spawn(move || {
                    std::mem::forget(r1.upgradable_read());
                });
                let t2 = spawn(move || {
                    let _g = rwlock.upgradable_read();
                });
                t1.join().unwrap();
                t2.join().unwrap();
            },
            None,
        );
    }

    #[test]
    fn upgrade_sanity() {
        check_dfs(
            move || {
                let rwlock = Arc::new(RwLock::new(0));
                let current_holders = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let current_holders1 = current_holders.clone();
                let r1 = rwlock.clone();
                let r2 = rwlock.clone();
                let t1 = spawn(move || {
                    let guard = r1.upgradable_read();
                    let holders = current_holders.fetch_add(1, Ordering::SeqCst);
                    assert!(holders == 0);
                    assert!(*guard < 2);
                    let mut write = RwLockUpgradableReadGuard::<'_, _>::upgrade(guard);
                    let holders = current_holders.fetch_sub(1, Ordering::SeqCst);
                    assert!(holders == 1);
                    *write += 1;
                });
                let t2 = spawn(move || {
                    let guard = r2.upgradable_read();
                    let holders = current_holders1.fetch_add(1, Ordering::SeqCst);
                    assert!(holders == 0, "{}", format!("{holders}"));
                    assert!(*guard < 2);
                    let mut write = RwLockUpgradableReadGuard::<'_, _>::upgrade(guard);
                    let holders = current_holders1.fetch_sub(1, Ordering::SeqCst);
                    assert!(holders == 1);
                    *write += 1;
                });
                t1.join().unwrap();
                t2.join().unwrap();
                assert!(*rwlock.read() == 2);
            },
            None,
        );
    }

    #[test]
    fn upgradable_read_does_not_block_write() {
        check_dfs(
            move || {
                let rwlock = Arc::new(RwLock::new(0));
                let r1 = rwlock.clone();
                let rg = rwlock.upgradable_read();
                let t2 = spawn(move || {
                    let _g = r1.write();
                });
                let g = RwLockUpgradableReadGuard::upgrade(rg);
                drop(g);
                t2.join().unwrap();
            },
            None,
        );
    }

    #[test]
    fn downgrade_write_to_read() {
        check_dfs(
            move || {
                let rwlock = Arc::new(RwLock::new(0));
                let mut w = rwlock.write();
                *w += 1;
                let r = RwLockWriteGuard::downgrade(w);
                // Still holding a read guard; the value we wrote is visible and a concurrent
                // reader can also acquire.
                assert_eq!(*r, 1);
                let r1 = rwlock.clone();
                let t = spawn(move || {
                    assert_eq!(*r1.read(), 1);
                });
                drop(r);
                t.join().unwrap();
            },
            None,
        );
    }

    #[test]
    fn writer_excludes_readers() {
        check_dfs(
            move || {
                let lock = Arc::new(RwLock::new(0i32));
                let w = lock.clone();
                let writer = spawn(move || {
                    let mut g = w.write();
                    // Transiently write an invalid value, then restore. The `yield_now` is a
                    // scheduling point: if `lock_exclusive` failed to exclude readers, a reader
                    // could be scheduled here and observe the `-1`.
                    *g = -1;
                    thread::yield_now();
                    *g = 1;
                });
                {
                    let r = lock.read();
                    assert!(*r == 0 || *r == 1, "reader observed writer's intermediate state");
                }
                writer.join().unwrap();
            },
            None,
        );
    }

    #[test]
    fn concurrent_readers_coexist() {
        check_dfs(
            move || {
                let lock = Arc::new(RwLock::new(0));
                let l2 = lock.clone();
                // Hold a read guard for the whole scope...
                let first = lock.read();
                let t = spawn(move || {
                    // ...a second reader must be able to acquire while the first is still held.
                    // If reads didn't share, this would block until `first` drops (after `join`),
                    // and Shuttle would report a deadlock.
                    let g = l2.read();
                    assert_eq!(*g, 0);
                });
                t.join().unwrap();
                drop(first);
            },
            None,
        );
    }

    #[test]
    fn try_semantics() {
        check_dfs(
            || {
                let lock = RwLock::new(0);
                {
                    let _r = lock.read();
                    assert!(lock.try_read().is_some(), "read should not exclude another read");
                    assert!(lock.try_write().is_none(), "read must exclude a write");
                }
                {
                    let _w = lock.write();
                    assert!(lock.try_read().is_none(), "write must exclude a read");
                    assert!(lock.try_write().is_none(), "write must exclude another write");
                }
                {
                    let _u = lock.upgradable_read();
                    assert!(lock.try_read().is_some(), "upgradable read should not exclude a read");
                    assert!(lock.try_write().is_none(), "upgradable read must exclude a write");
                    assert!(
                        lock.try_upgradable_read().is_none(),
                        "there may be at most one upgradable reader",
                    );
                }
            },
            None,
        );
    }

    #[test]
    fn try_upgrade_semantics() {
        check_dfs(
            || {
                // Succeeds when the upgradable reader is the only lock holder.
                {
                    let lock = RwLock::new(5);
                    let u = lock.upgradable_read();
                    let mut w = match RwLockUpgradableReadGuard::try_upgrade(u) {
                        Ok(w) => w,
                        Err(_) => panic!("try_upgrade should succeed when no readers are present"),
                    };
                    *w = 6;
                    assert_eq!(*w, 6);
                }
                // Fails while a plain reader is also held, and hands the upgradable guard back.
                {
                    let lock = RwLock::new(5);
                    let u = lock.upgradable_read();
                    let r = lock.read();
                    match RwLockUpgradableReadGuard::try_upgrade(u) {
                        Ok(_) => panic!("try_upgrade must fail while a reader is held"),
                        Err(u_back) => drop(u_back),
                    }
                    drop(r);
                }
            },
            None,
        );
    }

    #[test]
    fn downgrade_to_upgradable() {
        check_dfs(
            move || {
                let lock = Arc::new(RwLock::new(0));
                let mut w = lock.write();
                *w = 1;
                let u = RwLockWriteGuard::downgrade_to_upgradable(w);
                // The value written under exclusive access is visible through the upgradable guard.
                assert_eq!(*u, 1);
                // An upgradable reader still excludes writers...
                assert!(lock.try_write().is_none(), "upgradable read must exclude a write");
                // ...but permits plain readers.
                let r1 = lock.clone();
                let t = spawn(move || {
                    assert_eq!(*r1.read(), 1);
                });
                t.join().unwrap();
                drop(u);
            },
            None,
        );
    }

    #[test]
    fn upgradable_downgrade_to_read() {
        check_dfs(
            move || {
                let lock = Arc::new(RwLock::new(7));
                let u = lock.upgradable_read();
                let r = RwLockUpgradableReadGuard::downgrade(u);
                assert_eq!(*r, 7);
                // Downgrading released the upgradable slot, so another task may now take an
                // upgradable read; if the slot had leaked, this would deadlock.
                let l2 = lock.clone();
                let t = spawn(move || {
                    let _u2 = l2.upgradable_read();
                });
                t.join().unwrap();
                drop(r);
            },
            None,
        );
    }

    #[cfg(feature = "arc_lock")]
    #[test]
    fn arc_guards_reader_writer() {
        check_dfs(
            move || {
                let rwlock = Arc::new(RwLock::new(0usize));
                let w = rwlock.clone();
                let t = spawn(move || {
                    // `write_arc` returns an owned guard with a `'static` lifetime.
                    let mut g = w.write_arc();
                    *g += 1;
                });
                {
                    // `read_arc` likewise owns the `Arc`, so the guard has no borrow of `rwlock`.
                    let _g = rwlock.read_arc();
                }
                t.join().unwrap();
                assert_eq!(*rwlock.read(), 1);
            },
            None,
        );
    }
}
