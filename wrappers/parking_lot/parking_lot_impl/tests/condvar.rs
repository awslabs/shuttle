//! Shuttle's model of `parking_lot::Condvar`.

use shuttle::Config;
use shuttle::scheduler::{DfsScheduler, RandomScheduler};
use shuttle::sync::atomic::{AtomicBool, Ordering};
use shuttle::sync::mpsc::channel;
use shuttle::{Runner, check_dfs, check_random, check_uncontrolled_nondeterminism, current, thread};
use shuttle_parking_lot_impl::{Condvar, Mutex, MutexGuard, WaitTimeoutResult};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

type Pair = Arc<(Mutex<bool>, Condvar)>;

fn pair() -> Pair {
    Arc::new((Mutex::new(false), Condvar::new()))
}

/// `parking_lot`'s doc example: no spurious wakeups, so a plain `if` is enough.
#[test]
fn notify_one_wakes_the_waiter() {
    check_dfs(
        || {
            let pair = pair();
            let notifier = {
                let pair = Arc::clone(&pair);
                thread::spawn(move || {
                    let (lock, condvar) = &*pair;
                    *lock.lock() = true;
                    condvar.notify_one();
                })
            };
            let (lock, condvar) = &*pair;
            let mut started = lock.lock();
            if !*started {
                condvar.wait(&mut started);
                assert!(*started, "woken without a notification");
            }
            drop(started);
            notifier.join().unwrap();
        },
        None,
    );
}

/// A notification that comes after the waiter has unlocked the mutex is never lost, in any schedule.
#[test]
fn no_lost_wakeups() {
    check_dfs(
        || {
            let pair = pair();
            let waiters = (0..2)
                .map(|_| {
                    let pair = Arc::clone(&pair);
                    thread::spawn(move || {
                        let (lock, condvar) = &*pair;
                        let mut ready = lock.lock();
                        condvar.wait_while(&mut ready, |ready| !*ready);
                    })
                })
                .collect::<Vec<_>>();
            let (lock, condvar) = &*pair;
            *lock.lock() = true;
            condvar.notify_all();
            for waiter in waiters {
                waiter.join().unwrap();
            }
        },
        None,
    );
}

#[test]
fn notify_reports_whom_it_woke() {
    check_random(
        || {
            let pair = pair();
            let (lock, condvar) = &*pair;
            assert!(!condvar.notify_one());
            assert_eq!(condvar.notify_all(), 0);

            let (tx, rx) = channel();
            let waiters = (0..3)
                .map(|_| {
                    let pair = Arc::clone(&pair);
                    let tx = tx.clone();
                    thread::spawn(move || {
                        let (lock, condvar) = &*pair;
                        let mut woken = lock.lock();
                        tx.send(()).unwrap();
                        condvar.wait(&mut woken);
                    })
                })
                .collect::<Vec<_>>();
            for _ in 0..3 {
                rx.recv().unwrap();
            }
            // Each waiter told us with the lock held, so it is waiting once we hold it.
            let guard = lock.lock();
            assert!(condvar.notify_one());
            assert_eq!(condvar.notify_all(), 2);
            assert!(!condvar.notify_one());
            drop(guard);
            for waiter in waiters {
                waiter.join().unwrap();
            }
        },
        200,
    );
}

/// `notify_one` wakes the task that has waited longest, as `parking_lot` wakes the first thread in
/// its queue. The invariant holds in every schedule, so this is a random check rather than an
/// exhaustive one, which three waiters and the channels they report through make too large.
#[test]
fn notify_one_wakes_the_longest_waiter() {
    check_random(
        || {
            let pair = pair();
            let (lock, condvar) = &*pair;
            let (waiting_tx, waiting_rx) = channel();
            let (woke_tx, woke_rx) = channel();
            let waiters = (0..3)
                .map(|i| {
                    let (pair, waiting_tx, woke_tx) = (Arc::clone(&pair), waiting_tx.clone(), woke_tx.clone());
                    let waiter = thread::spawn(move || {
                        let (lock, condvar) = &*pair;
                        let mut guard = lock.lock();
                        waiting_tx.send(()).unwrap();
                        condvar.wait(&mut guard);
                        woke_tx.send(i).unwrap();
                    });
                    // Once the waiter has said so with the lock held, it waits as soon as the lock is
                    // free again: unlocking and waiting are one step.
                    waiting_rx.recv().unwrap();
                    drop(lock.lock());
                    waiter
                })
                .collect::<Vec<_>>();
            for i in 0..3 {
                assert!(condvar.notify_one());
                assert_eq!(woke_rx.recv().unwrap(), i);
            }
            for waiter in waiters {
                waiter.join().unwrap();
            }
        },
        200,
    );
}

/// Shuttle explores every order of the waiter and the notifier, including the one where the
/// notification comes first and the waiter must not wait.
#[test]
fn both_orders_are_explored() {
    let seen = Arc::new(std::sync::Mutex::new(BTreeSet::new()));
    let seen_clone = Arc::clone(&seen);
    check_dfs(
        move || {
            let pair = pair();
            let notifier = {
                let pair = Arc::clone(&pair);
                thread::spawn(move || {
                    let (lock, condvar) = &*pair;
                    *lock.lock() = true;
                    condvar.notify_one()
                })
            };
            let (lock, condvar) = &*pair;
            let mut ready = lock.lock();
            let waited = !*ready;
            condvar.wait_while(&mut ready, |ready| !*ready);
            drop(ready);
            let woke_someone = notifier.join().unwrap();
            assert_eq!(woke_someone, waited);
            seen_clone.lock().unwrap().insert(waited);
        },
        None,
    );
    assert_eq!(*seen.lock().unwrap(), BTreeSet::from([false, true]));
}

/// Waits with `wait`, with the flag set and a notification sent by another task at any point.
fn notified_by_another_task(wait: impl FnOnce(&Condvar, &mut MutexGuard<'_, bool>) -> Option<WaitTimeoutResult>) {
    let pair = pair();
    let notifier = {
        let pair = Arc::clone(&pair);
        thread::spawn(move || {
            let (lock, condvar) = &*pair;
            *lock.lock() = true;
            condvar.notify_one();
        })
    };
    let (lock, condvar) = &*pair;
    let mut ready = lock.lock();
    if let Some(result) = wait(condvar, &mut ready) {
        assert!(!result.timed_out());
    }
    assert!(*ready, "a wait returned without a notification");
    drop(ready);
    notifier.join().unwrap();
}

/// A timed wait never times out: it waits for the notification, however short the timeout.
#[test]
fn timed_waits_never_time_out() {
    type Wait = fn(&Condvar, &mut MutexGuard<'_, bool>) -> Option<WaitTimeoutResult>;
    let waits: [Wait; 4] = [
        |condvar, ready| Some(condvar.wait_while_for(ready, |ready| !*ready, Duration::ZERO)),
        |condvar, ready| Some(condvar.wait_while_until(ready, |ready| !*ready, Instant::now())),
        |condvar, ready| (!**ready).then(|| condvar.wait_for(ready, Duration::ZERO)),
        |condvar, ready| (!**ready).then(|| condvar.wait_until(ready, Instant::now())),
    ];
    for wait in waits {
        check_dfs(move || notified_by_another_task(wait), None);
    }
}

/// What a real run would end with a timeout, Shuttle reports as a deadlock.
#[test]
#[should_panic(expected = "deadlock")]
fn a_timed_wait_nobody_notifies_deadlocks() {
    check_dfs(
        || {
            let pair = pair();
            let (lock, condvar) = &*pair;
            let mut guard = lock.lock();
            let _ = condvar.wait_for(&mut guard, Duration::from_millis(1));
        },
        None,
    );
}

/// Whichever of the two waits comes second panics, as in `parking_lot`.
#[test]
#[should_panic(expected = "attempted to use a condition variable with more than one mutex")]
fn waiting_with_two_mutexes_panics() {
    check_dfs(
        || {
            let condvar = Arc::new(Condvar::new());
            let lock = Arc::new(Mutex::new(()));
            let (waiting_tx, waiting_rx) = channel();
            let first = {
                let (condvar, lock) = (Arc::clone(&condvar), Arc::clone(&lock));
                thread::spawn(move || {
                    let mut guard = lock.lock();
                    waiting_tx.send(()).unwrap();
                    condvar.wait(&mut guard);
                })
            };
            // The first task said so with its mutex held, and unlocking and waiting are one step, so
            // once this task has the mutex, the first one waits.
            waiting_rx.recv().unwrap();
            drop(lock.lock());
            let other = Mutex::new(());
            condvar.wait(&mut other.lock());
            first.join().unwrap();
        },
        None,
    );
}

/// Once nobody waits, the condvar can be used with another mutex.
#[test]
fn the_mutex_can_change_between_waits() {
    check_dfs(
        || {
            let condvar = Arc::new(Condvar::new());
            for _ in 0..2 {
                let lock = Arc::new(Mutex::new(false));
                let notifier = {
                    let (lock, condvar) = (Arc::clone(&lock), Arc::clone(&condvar));
                    thread::spawn(move || {
                        *lock.lock() = true;
                        condvar.notify_all();
                    })
                };
                let mut ready = lock.lock();
                condvar.wait_while(&mut ready, |ready| !*ready);
                drop(ready);
                notifier.join().unwrap();
            }
        },
        None,
    );
}

/// A notification is one scheduling point.
#[test]
fn context_switches() {
    check_dfs(
        || {
            let condvar = Condvar::new();
            let before = current::context_switches();
            condvar.notify_one();
            assert_eq!(current::context_switches() - before, 1);
            let before = current::context_switches();
            condvar.notify_all();
            assert_eq!(current::context_switches() - before, 1);
        },
        None,
    );
}

/// A wait's first scheduling point comes before it starts to wait, so a notification sent there,
/// by a task that doesn't hold the mutex, finds nobody waiting and is lost. Shuttle finds the
/// schedule; the waiter then waits forever.
#[test]
#[should_panic(expected = "deadlock")]
fn a_notification_before_the_wait_starts_is_lost() {
    check_dfs(
        || {
            let pair = pair();
            let ready = Arc::new(AtomicBool::new(false));
            let notifier = {
                let (pair, ready) = (Arc::clone(&pair), Arc::clone(&ready));
                thread::spawn(move || {
                    ready.store(true, Ordering::SeqCst);
                    // Wrongly, without the mutex.
                    pair.1.notify_one();
                })
            };
            let (lock, condvar) = &*pair;
            let mut guard = lock.lock();
            if !ready.load(Ordering::SeqCst) {
                condvar.wait(&mut guard);
            }
            drop(guard);
            notifier.join().unwrap();
        },
        None,
    );
}

/// A wait's last scheduling point comes before it locks the mutex again, so another task can take
/// the mutex first and change what the waiter was woken for. Shuttle finds the schedule, which is
/// why a waiter checks its condition in a loop.
#[test]
#[should_panic(expected = "the woken waiter found the item gone")]
fn another_task_can_lock_before_the_woken_waiter() {
    check_dfs(
        || {
            let state = Arc::new((Mutex::new(0usize), Condvar::new()));
            let waiter = {
                let state = Arc::clone(&state);
                thread::spawn(move || {
                    let (lock, condvar) = &*state;
                    let mut items = lock.lock();
                    if *items == 0 {
                        condvar.wait(&mut items);
                        assert!(*items > 0, "the woken waiter found the item gone");
                    }
                    *items -= 1;
                })
            };
            let thief = {
                let state = Arc::clone(&state);
                thread::spawn(move || {
                    let mut items = state.0.lock();
                    if *items > 0 {
                        *items -= 1;
                    }
                })
            };
            let (lock, condvar) = &*state;
            *lock.lock() += 1;
            condvar.notify_one();
            thief.join().unwrap();
            // Unblock the waiter if the thief took the item first.
            *lock.lock() += 1;
            condvar.notify_one();
            waiter.join().unwrap();
        },
        None,
    );
}

fn handshake_log() -> Vec<usize> {
    let pair = pair();
    let log = Arc::new(Mutex::new(Vec::new()));
    let workers = (0..3)
        .map(|i| {
            let (pair, log) = (Arc::clone(&pair), Arc::clone(&log));
            thread::spawn(move || {
                let (lock, condvar) = &*pair;
                let mut ready = lock.lock();
                condvar.wait_while(&mut ready, |ready| !*ready);
                log.lock().push(i);
            })
        })
        .collect::<Vec<_>>();
    let (lock, condvar) = &*pair;
    *lock.lock() = true;
    condvar.notify_all();
    for worker in workers {
        worker.join().unwrap();
    }
    let log = log.lock();
    log.clone()
}

#[test]
fn replays_from_a_seed() {
    let run = |seed| {
        let logs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let logs_clone = Arc::clone(&logs);
        Runner::new(RandomScheduler::new_from_seed(seed, 50), Default::default())
            .run(move || logs_clone.lock().unwrap().push(handshake_log()));
        Arc::try_unwrap(logs).unwrap().into_inner().unwrap()
    };
    let first = run(0x5EED);
    assert_eq!(first, run(0x5EED));
    assert_ne!(first, run(0xD1FF));
}

#[test]
fn uncontrolled_nondeterminism() {
    check_uncontrolled_nondeterminism(
        || {
            let _ = handshake_log();
        },
        100,
    );
}

#[test]
fn random_and_dfs_agree() {
    check_random(
        || {
            let _ = handshake_log();
        },
        200,
    );
    let executions = Runner::new(DfsScheduler::new(Some(5000), false), Default::default()).run(|| {
        let _ = handshake_log();
    });
    assert!(executions > 1);
}

/// The notifier's clock reaches the waiter it wakes, so what the notifier did happens before what
/// the waiter does after it wakes.
#[test]
fn a_notification_happens_before_the_wakeup() {
    check_random(
        || {
            let pair = pair();
            let (lock, condvar) = &*pair;
            let mut guard = lock.lock();
            let notifier = {
                let pair = Arc::clone(&pair);
                thread::spawn(move || {
                    let me = usize::from(current::me());
                    let seen = current::clock().get(me);
                    // Without the mutex, so the only causal link to the waiter is the notification.
                    while !pair.1.notify_one() {
                        thread::yield_now();
                    }
                    (me, seen)
                })
            };
            condvar.wait(&mut guard);
            let mine = current::clock();
            drop(guard);
            let (notifier, seen) = notifier.join().unwrap();
            assert!(mine.get(notifier) >= seen, "{mine:?}");
        },
        200,
    );
}

/// Teardown unwinds a task that is blocked in `wait` when the execution ends (see
/// `ExecutionState::tear_down`). Its destructors run, and the mutex is still exclusive: the guard
/// that the unwinding drops must not release the mutex that the wait already released.
#[test]
fn teardown_unwinds_a_waiting_task() {
    const EXECUTIONS: usize = 50;
    let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&completed);
    Runner::new(RandomScheduler::new(EXECUTIONS), Config::new()).run(move || {
        let pair = pair();
        let counter = Arc::clone(&counter);
        let (started_tx, started_rx) = channel();
        // A detached future task, so the execution ends while it waits. Teardown unwinds its stack,
        // dropping `on_drop` after the guard.
        drop(shuttle::future::spawn_local({
            let pair = Arc::clone(&pair);
            async move {
                let for_drop = Arc::clone(&pair);
                let on_drop = OnDrop(Some(move || {
                    let (lock, condvar) = &*for_drop;
                    let guard = lock.try_lock().expect("the wait released the mutex");
                    assert!(lock.try_lock().is_none(), "the mutex was unlocked twice");
                    drop(guard);
                    // The condvar is usable: this task is no longer waiting on it.
                    assert!(!condvar.notify_one());
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }));
                let mut guard = pair.0.lock();
                started_tx.send(()).unwrap();
                pair.1.wait(&mut guard);
                drop(on_drop);
            }
        }));
        started_rx.recv().unwrap();
    });
    assert_eq!(
        completed.load(std::sync::atomic::Ordering::SeqCst),
        EXECUTIONS,
        "a destructor did not run to completion in every execution"
    );
}

struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        (self.0.take().expect("dropped once"))();
    }
}
