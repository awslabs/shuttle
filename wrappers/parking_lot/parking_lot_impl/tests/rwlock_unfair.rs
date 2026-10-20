//! The unlocks of `RwLock`, which behave like those of `parking_lot` 0.12.5 (see "Modelling" in the
//! module docs of `raw_rwlock.rs`). A plain unlock wakes the tasks that `parking_lot`'s wake filter
//! unparks and leaves the lock free, so that another task can take it first, and a woken task that
//! finds it taken parks again, at the back of the queue. These tests show the deadlock that this
//! lets `parking_lot` reach, that no loop of unlocks starves a woken task, which unlocks hand the
//! lock over, and that `try_write` fails while a task is parked.
//!
//! Several tests need to know whether a task had parked when an unlock ran. A task that reads the
//! queue of parked tasks is causally after the parks that it reads, as in `parking_lot`, which
//! changes the queue only under the queue's lock: an unlock or a downgrade that finds tasks parked,
//! a `bump` that finds `PARKED_BIT` set (see `BatchSemaphore::has_waiters`), and a `try_write` that
//! `PARKED_BIT` makes fail. So the task that a test checks makes an event of its own before it asks
//! for the lock (see `spawn_after_event`), and right after the unlock, `after` says whether it had
//! parked. Like `tests/rwlock_clocks.rs`, the tests need the `vector-clocks` feature, which the
//! dev-dependency on `shuttle` turns on, and they record what they see with `std` types, which add
//! no scheduling points and no clock edges. `tests/mutex_unfair.rs` has the tests for `Mutex`.

use shuttle::scheduler::{DfsScheduler, PctScheduler};
use shuttle::sync::mpsc::channel;
use shuttle::thread::{self, JoinHandle};
use shuttle::{Config, Runner, check_dfs, check_pct, check_random_with_seed, current};
use shuttle_parking_lot_impl::{RwLock, RwLockUpgradableReadGuard, RwLockWriteGuard};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

/// Spawn a task that makes an event of its own and then runs `f`, and return its handle and its
/// task ID. The event is a store to a Shuttle atomic that no other task reads, so another task is
/// after it only through the lock, or by joining the task.
fn spawn_after_event<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> (JoinHandle<T>, usize) {
    let handle = thread::spawn(move || {
        shuttle::sync::atomic::AtomicBool::new(false).store(true, Ordering::SeqCst);
        f()
    });
    let id = usize::from(handle.thread().id());
    (handle, id)
}

/// `parking_lot`'s stranded reader, with `read_cycles` read locks that the main task takes and
/// releases between its write unlock and its upgradable read.
///
/// The main task holds a write lock while a writer and a reader ask for the lock. If the writer
/// parks first, the main task's unlock wakes only the writer: `parking_lot`'s wake filter
/// (`wake_parked_threads`) stops after the first writer that it unparks. The unlock leaves the lock
/// free (`unlock_exclusive_slow` returns `TOKEN_NORMAL`), and a request takes a free lock although
/// tasks are parked, so the main task can take its read locks and its upgradable read before the
/// writer tries again. The writer then finds the upgradable read, and parks again, behind the
/// reader. Now nothing wakes the reader, although the upgradable read would let it in:
/// `parking_lot` wakes parked readers only when `WRITER_BIT` or `UPGRADABLE_BIT` is cleared, and a
/// read unlock wakes only a writer that waits for the readers to leave (`unlock_shared`). The main
/// task waits for the reader while it holds the upgradable read, so the program deadlocks, in
/// `parking_lot` too.
fn strand_a_reader(read_cycles: usize, unlock: fn(RwLockWriteGuard<'_, ()>)) {
    let lock = Arc::new(RwLock::new(()));
    let (read_tx, read_rx) = channel();
    let write = lock.write();
    let writer = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || drop(lock.write()))
    };
    let reader = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || {
            let _read = lock.read();
            read_tx.send(()).unwrap();
        })
    };
    unlock(write);
    for _ in 0..read_cycles {
        drop(lock.read());
    }
    let upgradable = lock.upgradable_read();
    read_rx.recv().unwrap();
    drop(upgradable);
    writer.join().unwrap();
    reader.join().unwrap();
}

/// The stranded reader deadlocks, and Shuttle finds the schedule.
#[test]
#[should_panic(expected = "deadlock")]
fn stranded_reader() {
    check_dfs(|| strand_a_reader(0, |write| drop(write)), None);
}

/// With a read cycle between the write unlock and the upgradable read, the read unlock overtakes
/// the woken writer once, fewer times than the default `parking_lot_fairness_bound` (2), so Shuttle
/// still finds the deadlock.
#[test]
#[should_panic(expected = "deadlock")]
fn stranded_reader_after_a_read_cycle() {
    check_dfs(|| strand_a_reader(1, |write| drop(write)), None);
}

/// With two read cycles, the second read unlock is the second release that overtakes the woken
/// writer, so at the default bound it hands the writer the lock, in place of `parking_lot`'s
/// eventual fairness (see `Config::parking_lot_fairness_bound`), and the upgradable read waits for
/// the writer. So the bound limits what Shuttle explores: it leaves out the schedules in which a
/// woken request is overtaken more often, which `parking_lot` runs (a read cycle takes nanoseconds,
/// and a woken thread microseconds to run), and the deadlocks that need them. This test pins that
/// limit, which the module docs of `raw_rwlock.rs` and the README describe: exhaustive DFS passes.
#[test]
fn stranded_reader_after_two_read_cycles_is_missed_at_the_default_bound() {
    check_dfs(|| strand_a_reader(2, |write| drop(write)), None);
}

/// A larger bound explores the schedules in which the woken writer is overtaken twice, and finds
/// the deadlock.
#[test]
#[should_panic(expected = "deadlock")]
fn stranded_reader_after_two_read_cycles_is_found_with_a_larger_bound() {
    let mut config = Config::new();
    config.parking_lot_fairness_bound = 3;
    Runner::new(DfsScheduler::new(None, false), config).run(|| strand_a_reader(2, |write| drop(write)));
}

/// A fair write unlock hands the lock to the writer that it wakes (`unlock_exclusive_fair` calls
/// `unlock_exclusive_slow(true)`, which returns `TOKEN_HANDOFF`), so no upgradable read can get in
/// before the writer, and the writer's unlock wakes the reader: no reader is stranded.
#[test]
fn fair_write_unlock_strands_no_reader() {
    check_dfs(
        || strand_a_reader(0, |write| RwLockWriteGuard::unlock_fair(write)),
        None,
    );
}

/// `RwLockWriteGuard::unlock_fair` hands the lock to a parked writer (see
/// `fair_write_unlock_strands_no_reader`), so no task can take the lock before that writer: the
/// unlocking task's next read sees the writer's write. The unlock reads the queue, so it is after
/// the writer exactly when the writer had parked (see the module docs).
#[test]
fn write_unlock_fair_hands_the_lock_to_a_parked_writer() {
    static PARKED: AtomicBool = AtomicBool::new(false);
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(0));
            let write = lock.write();
            let (writer, writer_id) = {
                let lock = Arc::clone(&lock);
                spawn_after_event(move || *lock.write() += 1)
            };
            thread::yield_now();
            RwLockWriteGuard::unlock_fair(write);
            let parked = after(writer_id);
            let value = *lock.read();
            if parked {
                PARKED.store(true, Ordering::Relaxed);
                assert_eq!(
                    value, 1,
                    "a task took the lock before the writer that a fair unlock woke"
                );
            }
            writer.join().unwrap();
        },
        None,
    );
    assert!(PARKED.load(Ordering::Relaxed), "no schedule had the writer parked");
}

/// `RwLockUpgradableReadGuard::unlock_fair` is a plain unlock in `parking_lot` 0.12.5
/// (`unlock_upgradable_fair` calls `unlock_upgradable_slow(false)`), although `lock_api` documents
/// a fair unlock: it wakes a parked writer, but leaves the lock free, so the task that unlocked can
/// take the lock back before the writer tries again. The unlock reads the queue, so it is after the
/// writer exactly when the writer had parked (see the module docs).
#[test]
fn upgradable_unlock_fair_lets_the_unlocker_take_the_lock_back_first() {
    static TOOK_IT_BACK: AtomicBool = AtomicBool::new(false);
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(0));
            let upgradable = lock.upgradable_read();
            let (writer, writer_id) = {
                let lock = Arc::clone(&lock);
                spawn_after_event(move || *lock.write() += 1)
            };
            thread::yield_now();
            RwLockUpgradableReadGuard::unlock_fair(upgradable);
            let parked = after(writer_id);
            let value = *lock.upgradable_read();
            if parked && value == 0 {
                TOOK_IT_BACK.store(true, Ordering::Relaxed);
            }
            writer.join().unwrap();
        },
        None,
    );
    assert!(
        TOOK_IT_BACK.load(Ordering::Relaxed),
        "the unlocking task never took the lock back before the parked writer"
    );
}

/// `RwLockUpgradableReadGuard::bump` hands the lock to a parked writer (`bump_upgradable_slow`
/// unlocks with `unlock_upgradable_slow(true)`), unlike `unlock_fair`, so the upgradable reader
/// gets the lock back only after the writer: it sees the writer's write after the bump. The bump
/// reads `PARKED_BIT`, so it is after the writer exactly when the writer had parked (see the module
/// docs). Otherwise it keeps the lock.
#[test]
fn bump_upgradable_hands_the_lock_to_a_parked_writer() {
    static PARKED: AtomicBool = AtomicBool::new(false);
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(0));
            let mut upgradable = lock.upgradable_read();
            let (writer, writer_id) = {
                let lock = Arc::clone(&lock);
                spawn_after_event(move || *lock.write() += 1)
            };
            thread::yield_now();
            RwLockUpgradableReadGuard::bump(&mut upgradable);
            if after(writer_id) {
                PARKED.store(true, Ordering::Relaxed);
                assert_eq!(*upgradable, 1, "the bump did not hand the lock to the parked writer");
            }
            drop(upgradable);
            writer.join().unwrap();
        },
        None,
    );
    assert!(
        PARKED.load(Ordering::Relaxed),
        "no schedule had the writer parked at the bump"
    );
}

/// What a downgrade test records, over all of its schedules, about a task that had parked before
/// the downgrade: whether it got in while the downgrading task held the weaker lock, which shows
/// that the downgrade woke it, and whether it got in only after a write lock that was asked for
/// after the downgrade, which shows that the downgrade did not hand it the lock.
struct ParkedAtDowngrade {
    let_in: AtomicBool,
    overtaken: AtomicBool,
}

impl ParkedAtDowngrade {
    const fn new() -> Self {
        Self {
            let_in: AtomicBool::new(false),
            overtaken: AtomicBool::new(false),
        }
    }

    /// Record one schedule: whether the task had parked when the downgrade ran, whether it got in
    /// while the downgrading task held the weaker lock, and the value that it read, which is 1 if
    /// it got in after the write lock.
    fn record(&self, parked: bool, (let_in, value): (bool, usize)) {
        if parked {
            self.let_in.fetch_or(let_in, Ordering::Relaxed);
            self.overtaken.fetch_or(value == 1, Ordering::Relaxed);
        }
    }

    fn check(&self) {
        assert!(
            self.let_in.load(Ordering::Relaxed),
            "a task that had parked never got in beside the downgraded lock: the downgrade did not wake it"
        );
        assert!(
            self.overtaken.load(Ordering::Relaxed),
            "a task that had parked never got in after a later write lock: the downgrade handed it the lock"
        );
    }
}

/// `RwLockWriteGuard::downgrade` wakes the tasks that the read lock that it keeps lets in, and
/// hands them nothing (`downgrade_slow` returns `TOKEN_NORMAL`): a reader that parked behind the
/// write lock gets in while the downgrading task holds its read lock, or, if a writer asks for the
/// lock first and sets `WRITER_BIT`, parks again and gets in after that writer. The downgrade reads
/// the queue, so it is after the reader exactly when the reader had parked (see the module docs).
#[test]
fn downgrade_wakes_but_does_not_hand_off() {
    static SEEN: ParkedAtDowngrade = ParkedAtDowngrade::new();
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(0));
            let downgraded = Arc::new(AtomicBool::new(false));
            let write = lock.write();
            let (reader, reader_id) = {
                let (lock, downgraded) = (Arc::clone(&lock), Arc::clone(&downgraded));
                spawn_after_event(move || {
                    let read = lock.read();
                    (downgraded.load(Ordering::SeqCst), *read)
                })
            };
            thread::yield_now();
            let read = RwLockWriteGuard::downgrade(write);
            let parked = after(reader_id);
            downgraded.store(true, Ordering::SeqCst);
            let writer = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || *lock.write() += 1)
            };
            thread::yield_now();
            downgraded.store(false, Ordering::SeqCst);
            drop(read);
            writer.join().unwrap();
            SEEN.record(parked, reader.join().unwrap());
        },
        None,
    );
    SEEN.check();
}

/// `RwLockWriteGuard::downgrade_to_upgradable` wakes the readers that the upgradable read that it
/// keeps lets in, and hands them nothing (`downgrade_to_upgradable_slow` returns `TOKEN_NORMAL`): a
/// reader that parked behind the write lock gets in while the downgrading task holds its upgradable
/// read, or, if that task upgrades first and so sets `WRITER_BIT`, parks again and gets in after
/// the upgrade.
#[test]
fn downgrade_to_upgradable_wakes_but_does_not_hand_off() {
    static SEEN: ParkedAtDowngrade = ParkedAtDowngrade::new();
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(0));
            let downgraded = Arc::new(AtomicBool::new(false));
            let write = lock.write();
            let (reader, reader_id) = {
                let (lock, downgraded) = (Arc::clone(&lock), Arc::clone(&downgraded));
                spawn_after_event(move || {
                    let read = lock.read();
                    (downgraded.load(Ordering::SeqCst), *read)
                })
            };
            thread::yield_now();
            let upgradable = RwLockWriteGuard::downgrade_to_upgradable(write);
            let parked = after(reader_id);
            downgraded.store(true, Ordering::SeqCst);
            thread::yield_now();
            downgraded.store(false, Ordering::SeqCst);
            *RwLockUpgradableReadGuard::upgrade(upgradable) += 1;
            SEEN.record(parked, reader.join().unwrap());
        },
        None,
    );
    SEEN.check();
}

/// `RwLockUpgradableReadGuard::downgrade` wakes the tasks that the read lock that it keeps lets in,
/// and hands them nothing (`downgrade_upgradable` calls `downgrade_slow`): an upgradable reader
/// that parked behind the upgradable read gets in while the downgrading task holds its read lock,
/// or, if a writer asks for the lock first and sets `WRITER_BIT`, parks again and gets in after
/// that writer.
#[test]
fn downgrade_upgradable_wakes_but_does_not_hand_off() {
    static SEEN: ParkedAtDowngrade = ParkedAtDowngrade::new();
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(0));
            let downgraded = Arc::new(AtomicBool::new(false));
            let upgradable = lock.upgradable_read();
            let (other, other_id) = {
                let (lock, downgraded) = (Arc::clone(&lock), Arc::clone(&downgraded));
                spawn_after_event(move || {
                    let upgradable = lock.upgradable_read();
                    (downgraded.load(Ordering::SeqCst), *upgradable)
                })
            };
            thread::yield_now();
            let read = RwLockUpgradableReadGuard::downgrade(upgradable);
            let parked = after(other_id);
            downgraded.store(true, Ordering::SeqCst);
            let writer = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || *lock.write() += 1)
            };
            thread::yield_now();
            downgraded.store(false, Ordering::SeqCst);
            drop(read);
            writer.join().unwrap();
            SEEN.record(parked, other.join().unwrap());
        },
        None,
    );
    SEEN.check();
}

/// `try_write` fails while a task is parked, also on a free lock: `try_lock_exclusive` swaps the
/// lock word from 0, and `PARKED_BIT` is set. Two writers park behind the main task's write lock,
/// and its unlock wakes only the first (the wake filter stops after a writer), so the lock is free,
/// with the second writer parked, until the first tries again. The tasks count the holders of the
/// lock with a `std` atomic, which each updates with no scheduling point between it and the lock or
/// the unlock. (A `Mutex`'s `try_lock` takes a free lock although tasks are parked, see
/// `tests/mutex_unfair.rs`.)
#[test]
fn try_write_fails_on_a_free_lock_while_a_writer_is_parked() {
    static REFUSED: AtomicBool = AtomicBool::new(false);
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(()));
            let holders = Arc::new(AtomicUsize::new(1));
            let write = lock.write();
            let writers = (0..2)
                .map(|_| {
                    let (lock, holders) = (Arc::clone(&lock), Arc::clone(&holders));
                    thread::spawn(move || {
                        let write = lock.write();
                        holders.fetch_add(1, Ordering::SeqCst);
                        drop(write);
                        holders.fetch_sub(1, Ordering::SeqCst);
                    })
                })
                .collect::<Vec<_>>();
            thread::yield_now();
            drop(write);
            holders.fetch_sub(1, Ordering::SeqCst);
            if lock.try_write().is_none() && holders.load(Ordering::SeqCst) == 0 {
                REFUSED.store(true, Ordering::Relaxed);
            }
            for writer in writers {
                writer.join().unwrap();
            }
        },
        None,
    );
    assert!(
        REFUSED.load(Ordering::Relaxed),
        "try_write never failed on a free lock while a writer was parked"
    );
}

/// Let the tasks that the current task has spawned run until they park, in the first execution of
/// DFS and of PCT: PCT runs them before the current task once it yields, and DFS, which ignores
/// yields and runs the lowest-numbered task that can run, runs the empty task that this joins only
/// once the older tasks have parked. Other schedules can run this task first.
fn let_the_others_park() {
    for _ in 0..3 {
        thread::yield_now();
    }
    thread::spawn(|| {}).join().unwrap();
}

/// `spinners` tasks take a read lock in a loop until the flag that the lock guards is set, and a
/// writer, which parks behind them, sets it. The main task holds a write lock until they have
/// parked (see `let_the_others_park`).
///
/// A read unlock leaves the lock to the readers, and a spinner takes it again at once. Shuttle's
/// schedulers can keep a task that can run from running: DFS runs the lowest-numbered task that can
/// run, and PCT the one with the highest priority, which in its first execution is the oldest. So
/// once the writer is woken, they can keep running the spinners only, and keep the writer from
/// trying again, which an OS does not. In place of that, and of `parking_lot`'s eventual fairness,
/// Shuttle lets a woken request try once releases have overtaken it `parking_lot_fairness_bound`
/// times (see `Fairness::ParkingLot`): then the writer sets `WRITER_BIT`, which keeps the spinners
/// out. Without that, the first execution of DFS and of PCT exceeds `max_steps`.
fn read_spinners_and_a_writer(spinners: usize) {
    let lock = Arc::new(RwLock::new(false));
    let write = lock.write();
    let spinners = (0..spinners)
        .map(|_| {
            let lock = Arc::clone(&lock);
            thread::spawn(move || while !*lock.read() {})
        })
        .collect::<Vec<_>>();
    let writer = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || *lock.write() = true)
    };
    let_the_others_park();
    drop(write);
    for spinner in spinners {
        spinner.join().unwrap();
    }
    writer.join().unwrap();
}

/// The read spinners under DFS. DFS starts with the schedules in which every task has parked before
/// the main task unlocks, of which there are over 10,000 with two spinners, and more with more, so
/// the cap keeps it to those. After them, it goes on to schedules in which the main task unlocks
/// before the writer has asked for the lock, and the spinners keep it from asking at all, which no
/// lock can prevent (see the limitations in the README).
#[test]
fn read_spinners_cannot_starve_a_writer_under_dfs() {
    for spinners in 2..=4 {
        check_dfs(move || read_spinners_and_a_writer(spinners), Some(1_000));
    }
}

/// The read spinners under PCT: its first execution, and five seeds at depths 2 and 3.
#[test]
fn read_spinners_cannot_starve_a_writer_under_pct() {
    for spinners in 2..=4 {
        check_pct(move || read_spinners_and_a_writer(spinners), 1, 3);
        for seed in 0..5 {
            for depth in [2, 3] {
                let scheduler = PctScheduler::new_from_seed(seed, depth, 100);
                Runner::new(scheduler, Config::new()).run(move || read_spinners_and_a_writer(spinners));
            }
        }
    }
}

/// The read spinners under the random scheduler, which soon runs a woken task anyway, as an OS
/// does.
#[test]
fn read_spinners_cannot_starve_a_writer_under_random() {
    for spinners in 2..=4 {
        check_random_with_seed(move || read_spinners_and_a_writer(spinners), 0, 100);
    }
}

/// The main task takes the write lock back as soon as it unlocks it, until a writer that its unlock
/// woke has had the lock. DFS, which runs the lowest-numbered task that can run, takes it back
/// before the woken writer tries again, so without Shuttle's stand-in for `parking_lot`'s eventual
/// fairness (see `read_spinners_and_a_writer`), it would keep the writer out forever. The loop runs
/// only if the first unlock found the writer parked: the unlock reads the queue, so it is after the
/// writer exactly then (see the module docs). Before the writer asks for the lock, nothing can keep
/// DFS from running the loop only, so without that condition, exhaustive DFS would exceed
/// `max_steps`.
#[test]
fn relock_loop_cannot_starve_a_woken_writer() {
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(false));
            let write = lock.write();
            let (writer, writer_id) = {
                let lock = Arc::clone(&lock);
                spawn_after_event(move || *lock.write() = true)
            };
            thread::yield_now();
            drop(write);
            if after(writer_id) {
                while !*lock.write() {}
            }
            writer.join().unwrap();
        },
        None,
    );
}
