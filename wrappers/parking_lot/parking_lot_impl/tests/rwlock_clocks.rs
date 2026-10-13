//! Vector clocks: a task that takes the lock must be after the releases that let it in.
//!
//! Each test reads the current task's vector clock with `shuttle::current::clock()`. So the tests
//! need the `vector-clocks` feature, which the dev-dependency on `shuttle` turns on. The tests
//! record which task holds the lock with `std` types, which add no scheduling points and no clock
//! edges.

use shuttle::{check_dfs, current, thread};
use shuttle_parking_lot_impl::{RwLock, RwLockUpgradableReadGuard, RwLockWriteGuard};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

fn me() -> usize {
    usize::from(thread::current().id())
}

/// The latest event of `task` that happens before the current point of the current task, or 0.
fn time(task: usize) -> u32 {
    let clock = current::clock();
    let times: &[u32] = &clock;
    times.get(task).copied().unwrap_or(0)
}

/// Whether an event of `task` happens before the current point of the current task.
fn after(task: usize) -> bool {
    time(task) > 0
}

/// A writer writes its task ID under the lock. A reader that reads it is after the writer's
/// unlock. A reader that does not read it was let in before that unlock, so it must not be after
/// the writer.
fn check_reader_after_writer(read: fn(&RwLock<Option<usize>>) -> Option<Option<usize>>) {
    let lock = Arc::new(RwLock::new(None));
    let writer = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || *lock.write() = Some(me()))
    };
    let writer_id = usize::from(writer.thread().id());
    let reader = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || match read(&lock) {
            Some(Some(id)) => assert!(after(id), "the reader is not after the writer"),
            Some(None) => assert!(!after(writer_id), "the reader is after a writer that has not unlocked"),
            // A `try_read` that fails is checked by `failed_try_read_is_after_the_reserving_writer`.
            None => {}
        })
    };
    writer.join().unwrap();
    reader.join().unwrap();
}

#[test]
fn read_is_after_earlier_write() {
    check_dfs(|| check_reader_after_writer(|lock| Some(*lock.read())), None);
}

#[test]
fn try_read_is_after_earlier_write() {
    check_dfs(
        || check_reader_after_writer(|lock| lock.try_read().map(|guard| *guard)),
        None,
    );
}

#[test]
fn upgradable_read_is_after_earlier_write() {
    check_dfs(|| check_reader_after_writer(|lock| Some(*lock.upgradable_read())), None);
}

#[test]
fn try_upgradable_read_is_after_earlier_write() {
    check_dfs(
        || check_reader_after_writer(|lock| lock.try_upgradable_read().map(|guard| *guard)),
        None,
    );
}

/// Two readers each add their task ID to `held` while they hold the lock. When `take` returns an
/// exclusive lock, each reader in `held` has unlocked, so the task must be after it. A reader that
/// is not in `held` has not been let in yet, so the task must not be after it.
fn check_exclusive_after_readers(take: fn(&RwLock<()>) -> RwLockWriteGuard<'_, ()>) {
    let lock = Arc::new(RwLock::new(()));
    let held = Arc::new(Mutex::new(Vec::new()));
    let readers = (0..2)
        .map(|_| {
            let lock = Arc::clone(&lock);
            let held = Arc::clone(&held);
            thread::spawn(move || {
                let _guard = lock.read();
                held.lock().unwrap().push(me());
            })
        })
        .collect::<Vec<_>>();
    let reader_ids = readers
        .iter()
        .map(|reader| usize::from(reader.thread().id()))
        .collect::<Vec<_>>();
    {
        let _guard = take(&lock);
        let held = held.lock().unwrap();
        for id in reader_ids {
            assert_eq!(
                after(id),
                held.contains(&id),
                "reader {id}, readers that held the lock {held:?}"
            );
        }
    }
    for reader in readers {
        reader.join().unwrap();
    }
}

#[test]
fn write_is_after_earlier_reads() {
    check_dfs(|| check_exclusive_after_readers(|lock| lock.write()), None);
}

/// An upgrade of an upgradable read, which gives the upgradable read back when it fails.
type Upgrade = for<'a> fn(
    RwLockUpgradableReadGuard<'a, ()>,
) -> Result<RwLockWriteGuard<'a, ()>, RwLockUpgradableReadGuard<'a, ()>>;

/// The main task takes an upgradable read before it spawns two readers, which can hold the lock
/// with it. Then `upgrade` asks for an exclusive lock. When it gets one, each reader in `held` has
/// unlocked, so the task must be after it, and a reader that is not in `held` has not been let in
/// yet. When it fails, the task must not be after a reader that has not been let in yet.
fn check_upgrade_after_readers(upgrade: Upgrade) {
    let lock = Arc::new(RwLock::new(()));
    let upgradable = lock.upgradable_read();
    let held = Arc::new(Mutex::new(Vec::new()));
    let readers = (0..2)
        .map(|_| {
            let lock = Arc::clone(&lock);
            let held = Arc::clone(&held);
            thread::spawn(move || {
                let _guard = lock.read();
                held.lock().unwrap().push(me());
            })
        })
        .collect::<Vec<_>>();
    let reader_ids = readers
        .iter()
        .map(|reader| usize::from(reader.thread().id()))
        .collect::<Vec<_>>();
    match upgrade(upgradable) {
        Ok(_guard) => {
            let held = held.lock().unwrap();
            for id in reader_ids {
                assert_eq!(
                    after(id),
                    held.contains(&id),
                    "reader {id}, readers that held the lock {held:?}"
                );
            }
        }
        Err(_guard) => {
            let held = held.lock().unwrap();
            for id in reader_ids.into_iter().filter(|id| !held.contains(id)) {
                assert!(
                    !after(id),
                    "a failed try_upgrade is after reader {id}, which was not let in"
                );
            }
        }
    }
    for reader in readers {
        reader.join().unwrap();
    }
}

#[test]
fn upgrade_is_after_earlier_reads() {
    check_dfs(
        || check_upgrade_after_readers(|guard| Ok(RwLockUpgradableReadGuard::upgrade(guard))),
        None,
    );
}

#[test]
fn try_upgrade_is_after_earlier_reads() {
    check_dfs(
        || check_upgrade_after_readers(|guard| RwLockUpgradableReadGuard::try_upgrade(guard)),
        None,
    );
}

/// A writer writes its task ID, then downgrades with `downgrade` and keeps the weaker lock. A
/// reader that reads the ID is after the downgrade, and can be let in before the writer unlocks.
fn check_reader_after_downgrade(downgrade: fn(RwLockWriteGuard<'_, Option<usize>>)) {
    let lock = Arc::new(RwLock::new(None));
    let writer = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || {
            let mut guard = lock.write();
            *guard = Some(me());
            downgrade(guard);
        })
    };
    let writer_id = usize::from(writer.thread().id());
    let reader = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || match *lock.read() {
            Some(id) => assert!(after(id), "the reader is not after the downgrade"),
            None => assert!(!after(writer_id), "the reader is after a writer that has not released"),
        })
    };
    writer.join().unwrap();
    reader.join().unwrap();
}

#[test]
fn read_is_after_downgrade() {
    check_dfs(
        || {
            check_reader_after_downgrade(|guard| {
                let _guard = RwLockWriteGuard::downgrade(guard);
                thread::yield_now();
            })
        },
        None,
    );
}

#[test]
fn read_is_after_downgrade_to_upgradable() {
    check_dfs(
        || {
            check_reader_after_downgrade(|guard| {
                let _guard = RwLockWriteGuard::downgrade_to_upgradable(guard);
                thread::yield_now();
            })
        },
        None,
    );
}

/// `downgrade_upgradable` lets in an upgradable reader that waits, so that reader must be after the
/// downgrade. The main task reads its own clock just before the downgrade. Each release that can
/// let the reader in comes after that point, so the reader must be after a later event of the main
/// task. The `std` mutex adds no clock edges.
#[test]
fn upgradable_read_is_after_downgrade_upgradable() {
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(()));
            let upgradable = lock.upgradable_read();
            let main = me();
            let before = Arc::new(Mutex::new(None));
            let other = {
                let lock = Arc::clone(&lock);
                let before = Arc::clone(&before);
                thread::spawn(move || {
                    let _guard = lock.upgradable_read();
                    let before = before.lock().unwrap().expect("let in before the downgrade");
                    assert!(time(main) > before, "the upgradable reader is not after the downgrade");
                })
            };
            *before.lock().unwrap() = Some(time(main));
            let read = RwLockUpgradableReadGuard::downgrade(upgradable);
            thread::yield_now();
            drop(read);
            other.join().unwrap();
        },
        None,
    );
}

/// A `try_write` that fails is after the task that holds the lock, as for Shuttle's other
/// semaphore-based locks: a failed `try_*` joins the clock of the last successful acquire, as it
/// was when that task asked for the lock. Without this edge, `ReplayScheduler::set_target_clock`
/// could not replay the failure, because it would leave out the holder's steps. The holder locks a
/// Shuttle mutex first, so that its clock has an event of its own when it asks for the lock.
#[test]
fn failed_try_write_is_after_the_holder() {
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(()));
            let mutex = Arc::new(shuttle::sync::Mutex::new(()));
            let holder = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    drop(mutex.lock().unwrap());
                    let _guard = lock.write();
                    thread::yield_now();
                })
            };
            let holder_id = usize::from(holder.thread().id());
            let other = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    if lock.try_write().is_none() {
                        assert!(after(holder_id), "a failed try_write is not after the holder");
                    }
                })
            };
            holder.join().unwrap();
            other.join().unwrap();
        },
        None,
    );
}

/// A `try_read` that a waiting writer refuses (its reservation is `WRITER_BIT`) is after the writer,
/// although the writer does not hold the lock yet: a failed `try_*` also joins the clock of the
/// request that holds the reservation. The writer locks a Shuttle mutex first, so that its clock has
/// an event of its own when it asks for the lock.
#[test]
fn failed_try_read_is_after_the_reserving_writer() {
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(()));
            let read = lock.read();
            let mutex = Arc::new(shuttle::sync::Mutex::new(()));
            let writer = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    drop(mutex.lock().unwrap());
                    drop(lock.write());
                })
            };
            let writer_id = usize::from(writer.thread().id());
            let other = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    // The main task's read guard lets in every other reader, so only the writer
                    // can make this fail, by reserving the lock or by holding it.
                    if lock.try_read().is_none() {
                        assert!(after(writer_id), "a failed try_read is not after the writer");
                    }
                })
            };
            thread::yield_now();
            drop(read);
            writer.join().unwrap();
            other.join().unwrap();
        },
        None,
    );
}

/// `is_locked` and `is_locked_exclusive` load the lock state, so, like a load of an atomic word,
/// each is after the lock operations before it: after the holder's lock when it reads that a task
/// holds the lock, and after the unlock when it reads that the lock is free. Without these edges,
/// `ReplayScheduler::set_target_clock` could not replay a failure that depends on the result,
/// because it would leave out the holder's steps. The holder locks a Shuttle mutex first, so that
/// its clock has an event of its own when it asks for the lock. `unlocked` is a `std` atomic, which
/// adds no scheduling point and no clock edge, so it says whether the unlock happened before the
/// load.
fn check_load_after_holder(locked: fn(&RwLock<()>) -> bool) {
    let lock = Arc::new(RwLock::new(()));
    let mutex = Arc::new(shuttle::sync::Mutex::new(()));
    let unlocked = Arc::new(AtomicBool::new(false));
    let holder = {
        let (lock, unlocked) = (Arc::clone(&lock), Arc::clone(&unlocked));
        thread::spawn(move || {
            drop(mutex.lock().unwrap());
            drop(lock.write());
            unlocked.store(true, Ordering::SeqCst);
        })
    };
    let holder_id = usize::from(holder.thread().id());
    let other = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || {
            let locked = locked(&lock);
            if locked || unlocked.load(Ordering::SeqCst) {
                assert!(after(holder_id), "the load is not after the holder (locked: {locked})");
            } else {
                assert!(
                    !after(holder_id),
                    "the load is after a holder that has not asked for the lock"
                );
            }
        })
    };
    holder.join().unwrap();
    other.join().unwrap();
}

#[test]
fn is_locked_is_after_the_holder() {
    check_dfs(|| check_load_after_holder(|lock| lock.is_locked()), None);
}

#[test]
fn is_locked_exclusive_is_after_the_holder() {
    check_dfs(|| check_load_after_holder(|lock| lock.is_locked_exclusive()), None);
}
