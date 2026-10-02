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
    use std::{
        collections::HashSet,
        sync::{Arc, atomic::Ordering},
    };

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

    // An upgrade is atomic with respect to writers: a writer blocked when the upgrade starts cannot
    // be granted the lock first, so the data an upgradable reader observed cannot change across its
    // own upgrade. That guarantee is the whole point of an upgradable read.
    #[test]
    fn upgrade_excludes_blocked_writer() {
        check_dfs(
            move || {
                let rwlock = Arc::new(RwLock::new(0));
                let r1 = rwlock.clone();
                let t = spawn(move || {
                    *r1.write() += 1;
                });
                let u = rwlock.upgradable_read();
                let observed = *u;
                let w = RwLockUpgradableReadGuard::upgrade(u);
                assert_eq!(*w, observed, "a writer was granted the lock during the upgrade");
                drop(w);
                t.join().unwrap();
            },
            None,
        );
    }

    // Same guarantee, but on the path where the upgrade has to block: a plain reader is in the lock,
    // so the upgrade waits for it to leave. It must still be granted ahead of the waiting writer.
    #[test]
    fn upgrade_waits_for_reader_then_precedes_writer() {
        // What the plain reader saw, across all executions. The reader is ordered against the writer
        // only by the schedule, so both values must show up: `{0}` alone would mean the schedule
        // where the reader runs after the writer was never explored, and the test would not actually
        // be exercising the interesting case.
        let reader_observed = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let reader_observed_clone = Arc::clone(&reader_observed);

        check_dfs(
            move || {
                let rwlock = Arc::new(RwLock::new(0));
                let (r1, r2) = (rwlock.clone(), rwlock.clone());
                let reader_observed = Arc::clone(&reader_observed_clone);
                let writer = spawn(move || {
                    *r1.write() += 1;
                });
                let reader = spawn(move || {
                    let g = r2.read();
                    reader_observed.lock().unwrap().insert(*g);
                });
                let u = rwlock.upgradable_read();
                let observed = *u;
                let w = RwLockUpgradableReadGuard::upgrade(u);
                assert_eq!(*w, observed, "a writer was granted the lock during the upgrade");
                drop(w);
                reader.join().unwrap();
                writer.join().unwrap();
            },
            None,
        );

        assert_eq!(
            *reader_observed.lock().unwrap(),
            HashSet::from([0, 1]),
            "the reader should have observed the value both before and after the writer's increment",
        );
    }

    // A blocked writer does not make `try_upgrade` fail: it holds nothing, and `parking_lot`'s
    // `try_upgrade` only cares whether other *readers* are in the lock.
    #[test]
    fn try_upgrade_ignores_blocked_writer() {
        check_dfs(
            move || {
                let lock = Arc::new(RwLock::new(0));
                let l2 = lock.clone();
                let u = lock.upgradable_read();
                let t = spawn(move || {
                    let _w = l2.write();
                });
                match RwLockUpgradableReadGuard::try_upgrade(u) {
                    Ok(w) => drop(w),
                    Err(_) => panic!("try_upgrade must succeed when no other reader holds the lock"),
                }
                t.join().unwrap();
            },
            None,
        );
    }

    // `downgrade_to_upgradable` cannot block, so a task waiting for an upgradable read cannot
    // deadlock against it. In `parking_lot` the downgrade is an atomic `WRITER_BIT` -> `ONE_READER |
    // UPGRADABLE_BIT` swap, and a task merely waiting for an upgradable read holds nothing.
    #[test]
    fn downgrade_to_upgradable_with_waiting_upgradable_reader() {
        check_dfs(
            move || {
                let lock = Arc::new(RwLock::new(0));
                let l2 = lock.clone();
                let mut w = lock.write();
                let t = spawn(move || {
                    let _u = l2.upgradable_read();
                });
                *w = 1;
                let u = RwLockWriteGuard::downgrade_to_upgradable(w);
                drop(u);
                t.join().unwrap();
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

    /// Run a test in which the main task panics with "the original panic" while another task holds
    /// a read lock. While the main task unwinds, it first releases its own read lock, and then it
    /// drops the value that `on_drop` made. The release closes the lock (see "Stopped executions" in
    /// the `raw_rwlock` module docs).
    fn panic_while_another_task_reads<D: 'static>(on_drop: fn(Arc<RwLock<()>>) -> D) {
        check_dfs(
            move || {
                let lock = Arc::new(RwLock::new(()));
                let gate = Arc::new(shuttle::sync::Mutex::new(()));
                let reading = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
                let held_gate = gate.lock().unwrap();
                let other = {
                    let (lock, gate, reading) = (lock.clone(), gate.clone(), reading.clone());
                    spawn(move || {
                        let _read = lock.read();
                        reading.store(true, Ordering::SeqCst);
                        // Keep the read lock until the main task releases `gate`.
                        let _gate = gate.lock();
                    })
                };
                let _on_drop = on_drop(lock.clone());
                let own_read = lock.read();
                if reading.load(Ordering::SeqCst) {
                    panic!("the original panic");
                }
                drop(own_read);
                drop(held_gate);
                other.join().unwrap();
            },
            None,
        );
    }

    /// While the main task unwinds, a `Drop` asks for a write lock that the other task's read lock
    /// blocks. The lock is closed, so the request returns at once, and Shuttle reports the panic and
    /// not a deadlock.
    #[test]
    #[should_panic = "the original panic"]
    fn unwinding_task_takes_a_held_lock() {
        struct WriteOnDrop(Arc<RwLock<()>>);

        impl Drop for WriteOnDrop {
            fn drop(&mut self) {
                let _guard = self.0.write();
            }
        }

        panic_while_another_task_reads(WriteOnDrop);
    }

    /// On a closed lock, a `try_*` fails, and an unlock does not make the lock look free. A failed
    /// check in a `Drop` during the unwind would abort the process, so the `Drop` records the
    /// results, and the test checks them after the panic.
    #[test]
    fn closed_lock_refuses_try_and_ignores_unlock() {
        use std::sync::atomic::AtomicBool;
        static TRY_READ_REFUSED: AtomicBool = AtomicBool::new(true);
        static STILL_LOCKED: AtomicBool = AtomicBool::new(true);

        struct CheckOnDrop(Arc<RwLock<()>>);

        impl Drop for CheckOnDrop {
            fn drop(&mut self) {
                // In a schedule that does not panic, the lock is not closed.
                if !std::thread::panicking() {
                    return;
                }
                // The lock has no writer, so only the closed lock refuses this request.
                if self.0.try_read().is_some() {
                    TRY_READ_REFUSED.store(false, Ordering::SeqCst);
                }
                // This request returns without the lock, and its unlock must not make the lock look
                // free: the other task still holds a read lock.
                drop(self.0.upgradable_read());
                if !self.0.is_locked() {
                    STILL_LOCKED.store(false, Ordering::SeqCst);
                }
            }
        }

        let panic = std::panic::catch_unwind(|| panic_while_another_task_reads(CheckOnDrop))
            .expect_err("the test did not panic");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(message.contains("the original panic"), "unexpected panic: {message}");
        assert!(
            TRY_READ_REFUSED.load(Ordering::SeqCst),
            "try_read was granted on a closed lock"
        );
        assert!(
            STILL_LOCKED.load(Ordering::SeqCst),
            "an unlock made a closed lock look free"
        );
    }

    #[test]
    fn is_locked_in_each_mode() {
        check_dfs(
            || {
                let lock = RwLock::new(());
                assert!(!lock.is_locked() && !lock.is_locked_exclusive());
                {
                    let _guard = lock.read();
                    assert!(lock.is_locked() && !lock.is_locked_exclusive());
                }
                {
                    let _guard = lock.upgradable_read();
                    assert!(lock.is_locked() && !lock.is_locked_exclusive());
                }
                {
                    let _guard = lock.write();
                    assert!(lock.is_locked() && lock.is_locked_exclusive());
                }
                assert!(!lock.is_locked() && !lock.is_locked_exclusive());
            },
            None,
        );
    }

    /// A writer that waits for a reader to leave holds `WRITER_BIT`, so `is_locked_exclusive` is
    /// true before the writer has the lock, as in `parking_lot`.
    #[test]
    fn is_locked_exclusive_while_a_writer_waits() {
        static SAW_WAITING_WRITER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        check_dfs(
            || {
                let lock = Arc::new(RwLock::new(()));
                let requested = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
                let read = lock.read();
                let writer = {
                    let (lock, requested) = (lock.clone(), requested.clone());
                    spawn(move || {
                        requested.store(true, Ordering::SeqCst);
                        drop(lock.write());
                    })
                };
                // `is_locked_exclusive` is not a scheduling point, so yield to let the writer run. The
                // main task holds a read lock, so the writer cannot have the lock yet.
                thread::yield_now();
                if lock.is_locked_exclusive() {
                    assert!(
                        requested.load(Ordering::SeqCst),
                        "WRITER_BIT is set, but no task asked to write"
                    );
                    SAW_WAITING_WRITER.store(true, Ordering::Relaxed);
                }
                drop(read);
                writer.join().unwrap();
            },
            None,
        );
        assert!(
            SAW_WAITING_WRITER.load(Ordering::Relaxed),
            "no schedule had a waiting writer"
        );
    }
}

#[cfg(test)]
mod parking_lot_upgrade_parity {
    use super::{RwLock, RwLockUpgradableReadGuard};
    use shuttle::{
        check_dfs,
        thread::{self, spawn},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    /// Test 1: two tasks each take an upgradable read, observe the value, upgrade, and increment.
    ///
    /// `parking_lot` prints `GETTING / UPGRADING / UPGRADED / GOT`: the second task's
    /// `upgradable_read()` is not granted until the first has finished upgrading. The `sleep`s in
    /// the original only select *one* interleaving; the properties that hold for *every*
    /// interleaving are asserted below, and `check_dfs` checks them on all of them.
    #[test]
    fn upgradable_read_is_exclusive_and_upgrade_is_atomic() {
        check_dfs(
            || {
                let rwlock = Arc::new(RwLock::new(0));
                // Counts tasks inside the upgradable-read..upgraded region. `parking_lot` admits at
                // most one, so this must read 0 on entry and 1 on exit -- the original's
                // `current_holders`.
                let holders = Arc::new(AtomicUsize::new(0));

                let threads = (0..2)
                    .map(|_| {
                        let rwlock = Arc::clone(&rwlock);
                        let holders = Arc::clone(&holders);
                        spawn(move || {
                            let guard = rwlock.upgradable_read();

                            // At most one upgradable reader: `parking_lot` has a single
                            // UPGRADABLE_BIT. This is what makes `GOT` come after `UPGRADED`.
                            assert_eq!(
                                holders.fetch_add(1, Ordering::SeqCst),
                                0,
                                "two tasks held an upgradable read at once",
                            );
                            // Stands in for the original's `sleep`: offer the scheduler a
                            // preemption point here, so DFS will try to interleave the other task
                            // into this region if the lock lets it.
                            thread::yield_now();

                            // An upgradable read excludes writers, so the other task cannot have
                            // incremented twice while we hold it.
                            let observed = *guard;
                            assert!(observed < 2, "the value changed under an upgradable read");

                            let mut write = RwLockUpgradableReadGuard::upgrade(guard);

                            // The upgrade is atomic with respect to writers: what we read through
                            // the upgradable guard cannot change across our own upgrade. This is
                            // the guarantee that makes an upgradable read worth having, and the one
                            // a naive "drop the read, then take the write" upgrade breaks.
                            assert_eq!(*write, observed, "a writer was granted the lock during the upgrade");

                            // ...and the upgrade never let go of the lock, so we are still the only
                            // holder on the way out.
                            assert_eq!(
                                holders.fetch_sub(1, Ordering::SeqCst),
                                1,
                                "another task entered the lock during the upgrade",
                            );

                            *write += 1;
                        })
                    })
                    .collect::<Vec<_>>();

                for t in threads {
                    t.join().unwrap();
                }

                // Both upgrades were granted -- neither deadlocked, and no update was lost.
                assert_eq!(*rwlock.read(), 2);
            },
            None,
        );
    }

    /// Test 2: the main task holds an upgradable read while another task blocks in `write()`, then
    /// upgrades.
    #[test]
    fn upgrade_precedes_an_already_waiting_writer() {
        check_dfs(
            || {
                let rwlock = Arc::new(RwLock::new(0));
                let writer_finished = Arc::new(AtomicUsize::new(0));

                let upgradable = rwlock.upgradable_read();
                let observed = *upgradable;

                let writer = {
                    let rwlock = Arc::clone(&rwlock);
                    let writer_finished = Arc::clone(&writer_finished);
                    spawn(move || {
                        *rwlock.write() += 1;
                        writer_finished.store(1, Ordering::SeqCst);
                    })
                };

                // Stands in for the `sleep` that let the writer reach `write()` and block. DFS also
                // explores the schedules where it has not got there yet.
                thread::yield_now();

                let write = RwLockUpgradableReadGuard::upgrade(upgradable);

                assert_eq!(
                    writer_finished.load(Ordering::SeqCst),
                    0,
                    "the waiting writer was granted the lock before the upgrade",
                );
                assert_eq!(*write, observed, "a writer was granted the lock during the upgrade");
                drop(write);

                writer.join().unwrap();
                assert_eq!(*rwlock.read(), 1);
            },
            None,
        );
    }
}
