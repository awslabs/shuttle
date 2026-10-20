//! The unlocks of `Mutex`, which behave like those of `parking_lot` 0.12.5 (see `raw_mutex.rs`). A
//! plain unlock wakes the task that has waited longest and leaves the lock free, so that another
//! task can take it first, and the woken task parks again, at the back of the queue, if it finds
//! the lock taken. `unlock_fair` and `bump` hand the lock to it instead. These tests show that no
//! loop of unlocks starves a woken task, and pin down what `unlock_fair`, `bump` and `try_lock` do
//! while a task is parked or woken.
//!
//! Several tests need to know whether a task had parked when an unlock ran. An unlock that finds
//! tasks parked reads the queue, which `parking_lot` changes only under the queue's lock, so the
//! unlocking task is causally after the parks that it reads. So the task that a test checks makes
//! an event of its own before it asks for the lock (see `spawn_after_event`), and right after the
//! unlock, `after` says whether it had parked. The tests need the `vector-clocks` feature, which
//! the dev-dependency on `shuttle` turns on, and they record what they see with `std` types, which
//! add no scheduling points and no clock edges. `tests/rwlock_unfair.rs` has the tests for
//! `RwLock`.

use shuttle::scheduler::PctScheduler;
use shuttle::thread::{self, JoinHandle};
use shuttle::{Config, Runner, check_dfs, check_pct, check_random_with_seed, current};
use shuttle_parking_lot_impl::{Mutex, MutexGuard};
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

/// `MutexGuard::unlock_fair` hands the lock to the task that has waited longest
/// (`unlock_slow(true)` returns `TOKEN_HANDOFF`), so no task can take the lock before that task:
/// the unlocking task's next lock sees the waiter's write. The unlock reads the queue, so it is
/// after the waiter exactly when the waiter had parked (see the module docs).
#[test]
fn unlock_fair_hands_the_lock_to_a_parked_waiter() {
    static PARKED: AtomicBool = AtomicBool::new(false);
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(0));
            let guard = lock.lock();
            let (waiter, waiter_id) = {
                let lock = Arc::clone(&lock);
                spawn_after_event(move || *lock.lock() += 1)
            };
            thread::yield_now();
            MutexGuard::unlock_fair(guard);
            let parked = after(waiter_id);
            let value = *lock.lock();
            if parked {
                PARKED.store(true, Ordering::Relaxed);
                assert_eq!(
                    value, 1,
                    "a task took the lock before the waiter that a fair unlock woke"
                );
            }
            waiter.join().unwrap();
        },
        None,
    );
    assert!(PARKED.load(Ordering::Relaxed), "no schedule had the waiter parked");
}

/// `try_lock` takes a free lock although tasks are parked, as `parking_lot`'s does, which looks
/// only at `LOCKED_BIT` (unlike `RwLock::try_write`, which fails while `PARKED_BIT` is set, see
/// `tests/rwlock_unfair.rs`). Two tasks park behind the main task, whose unlock wakes only the one
/// that parked first (`unlock_slow` unparks one thread), so the other is still parked while the
/// lock is free, until the woken task has had the lock. The unlock reads the queue, so it is after
/// both exactly when both had parked (see the module docs), and the tasks count the times that they
/// have had the lock with a `std` atomic, which each updates with no scheduling point between it
/// and the lock.
#[test]
fn try_lock_takes_a_free_lock_while_a_waiter_is_parked() {
    static TOOK_IT: AtomicBool = AtomicBool::new(false);
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(()));
            let had_it = Arc::new(AtomicUsize::new(0));
            let guard = lock.lock();
            let (waiters, ids): (Vec<_>, Vec<_>) = (0..2)
                .map(|_| {
                    let (lock, had_it) = (Arc::clone(&lock), Arc::clone(&had_it));
                    spawn_after_event(move || {
                        let guard = lock.lock();
                        had_it.fetch_add(1, Ordering::SeqCst);
                        drop(guard);
                    })
                })
                .unzip();
            thread::yield_now();
            drop(guard);
            let both_parked = ids.iter().all(|&id| after(id));
            if lock.try_lock().is_some() && both_parked && had_it.load(Ordering::SeqCst) == 0 {
                TOOK_IT.store(true, Ordering::Relaxed);
            }
            for waiter in waiters {
                waiter.join().unwrap();
            }
        },
        None,
    );
    assert!(
        TOOK_IT.load(Ordering::Relaxed),
        "try_lock never took the lock while a waiter was parked"
    );
}

/// `MutexGuard::bump` unlocks only while `PARKED_BIT` is set (`RawMutex::bump`). The unlock that
/// wakes the only parked task clears the bit (`unlock_slow` stores 0, as `have_more_threads` is
/// false), so when the main task takes the lock back before the woken task tries again, a bump
/// keeps the lock, until the woken task has tried, found the lock taken, and parked again. The
/// unlock reads the queue, so it is after the waiter exactly when it found the waiter parked and
/// woke it (see the module docs). Another task's `try_lock` checks that the bump never frees the
/// lock before the waiter has had it: the lock is free during the bump only after the bump has
/// handed it to the waiter, which then unlocked.
#[test]
fn bump_keeps_the_lock_while_the_only_waiter_is_woken() {
    static KEPT: AtomicBool = AtomicBool::new(false);
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(0));
            let in_bump = Arc::new(AtomicBool::new(false));
            let guard = lock.lock();
            let (waiter, waiter_id) = {
                let lock = Arc::clone(&lock);
                spawn_after_event(move || *lock.lock() += 1)
            };
            thread::yield_now();
            drop(guard);
            let woken = after(waiter_id);
            let other = {
                let (lock, in_bump) = (Arc::clone(&lock), Arc::clone(&in_bump));
                thread::spawn(move || {
                    if let Some(guard) = lock.try_lock() {
                        let in_bump = in_bump.load(Ordering::SeqCst);
                        assert!(
                            !in_bump || *guard == 1,
                            "a bump freed the lock before the waiter had it"
                        );
                    }
                })
            };
            let mut guard = lock.lock();
            let before = *guard;
            in_bump.store(true, Ordering::SeqCst);
            MutexGuard::bump(&mut guard);
            in_bump.store(false, Ordering::SeqCst);
            if woken && before == 0 && *guard == 0 {
                KEPT.store(true, Ordering::Relaxed);
            }
            drop(guard);
            waiter.join().unwrap();
            other.join().unwrap();
        },
        None,
    );
    assert!(
        KEPT.load(Ordering::Relaxed),
        "no bump kept the lock while the waiter was woken and had not tried again"
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

/// `spinners` tasks take the lock in a loop until the flag that it guards is set, and a waiter,
/// which parks behind them, sets it. The main task holds the lock until they have parked (see
/// `let_the_others_park`).
///
/// An unlock leaves the lock free, and a spinner takes it again at once. Shuttle's schedulers can
/// keep a task that can run from running: DFS runs the lowest-numbered task that can run, and PCT
/// the one with the highest priority, which in its first execution is the oldest. So once the
/// waiter is woken, they can keep running the spinners only, and keep the waiter from trying
/// again, which an OS does not. In place of that, and of `parking_lot`'s eventual fairness, Shuttle
/// hands the lock to a woken request once releases have overtaken it `parking_lot_fairness_bound`
/// times (see `Fairness::ParkingLot`). Without that, the first execution of DFS and of PCT exceeds
/// `max_steps`.
fn spinners_and_a_waiter(spinners: usize) {
    let lock = Arc::new(Mutex::new(false));
    let guard = lock.lock();
    let spinners = (0..spinners)
        .map(|_| {
            let lock = Arc::clone(&lock);
            thread::spawn(move || while !*lock.lock() {})
        })
        .collect::<Vec<_>>();
    let waiter = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || *lock.lock() = true)
    };
    let_the_others_park();
    drop(guard);
    for spinner in spinners {
        spinner.join().unwrap();
    }
    waiter.join().unwrap();
}

/// The spinners under DFS. DFS starts with the schedules in which every task has parked before the
/// main task unlocks, of which there are over 10,000 with two spinners, and more with more, so the
/// cap keeps it to those. After them, it goes on to schedules in which the main task unlocks before
/// the waiter has asked for the lock, and the spinners keep it from asking at all, which no lock
/// can prevent (see the limitations in the README).
#[test]
fn spinners_cannot_starve_a_waiter_under_dfs() {
    for spinners in 2..=4 {
        check_dfs(move || spinners_and_a_waiter(spinners), Some(1_000));
    }
}

/// The spinners under PCT: its first execution, and five seeds at depths 2 and 3.
#[test]
fn spinners_cannot_starve_a_waiter_under_pct() {
    for spinners in 2..=4 {
        check_pct(move || spinners_and_a_waiter(spinners), 1, 3);
        for seed in 0..5 {
            for depth in [2, 3] {
                let scheduler = PctScheduler::new_from_seed(seed, depth, 100);
                Runner::new(scheduler, Config::new()).run(move || spinners_and_a_waiter(spinners));
            }
        }
    }
}

/// The spinners under the random scheduler, which soon runs a woken task anyway, as an OS does.
#[test]
fn spinners_cannot_starve_a_waiter_under_random() {
    for spinners in 2..=4 {
        check_random_with_seed(move || spinners_and_a_waiter(spinners), 0, 100);
    }
}

/// The main task takes the lock back as soon as it unlocks it, until a waiter that its unlock woke
/// has had the lock. DFS, which runs the lowest-numbered task that can run, takes it back before
/// the woken waiter tries again, so without Shuttle's stand-in for `parking_lot`'s eventual
/// fairness (see `spinners_and_a_waiter`), it would keep the waiter out forever. The loop runs only
/// if the first unlock found the waiter parked: the unlock reads the queue, so it is after the
/// waiter exactly then (see the module docs). Before the waiter asks for the lock, nothing can keep
/// DFS from running the loop only, so without that condition, exhaustive DFS would exceed
/// `max_steps`.
#[test]
fn relock_loop_cannot_starve_a_woken_waiter() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(false));
            let guard = lock.lock();
            let (waiter, waiter_id) = {
                let lock = Arc::clone(&lock);
                spawn_after_event(move || *lock.lock() = true)
            };
            thread::yield_now();
            drop(guard);
            if after(waiter_id) {
                while !*lock.lock() {}
            }
            waiter.join().unwrap();
        },
        None,
    );
}
