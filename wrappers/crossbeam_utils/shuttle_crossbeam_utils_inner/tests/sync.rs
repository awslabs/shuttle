//! `ShardedLock`, `WaitGroup` and `Parker` under Shuttle: blocking, the scheduler's choices,
//! poisoning, and the rule that a timed park times out only when no task can run.

mod common;

use common::random_outcomes;
use shuttle::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_dfs, check_random, thread};
use shuttle_crossbeam_utils_inner::sync::{Parker, ShardedLock, WaitGroup};
use shuttle_time_inner::time::{Duration, Instant};
use shuttle_time_inner::timer_support::peek;

const HOUR: Duration = Duration::from_secs(3600);
const MS: Duration = Duration::from_millis(1);

/// An exhaustive search, bounded, for a test whose state space is too large to finish: the
/// `Condvar` that the wait group and the parker use wakes every waiter and lets them race.
fn check_dfs_bounded(f: impl Fn() + Send + Sync + 'static, _unbounded: Option<usize>) {
    shuttle::check_dfs(f, Some(5_000));
}

#[test]
fn readers_see_a_writers_value_and_never_a_half_written_one() {
    check_dfs(
        || {
            let lock = Arc::new(ShardedLock::new((0u32, 0u32)));
            let writer = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let mut guard = lock.write().unwrap();
                    guard.0 += 1;
                    thread::yield_now();
                    guard.1 += 1;
                })
            };
            {
                let guard = lock.read().unwrap();
                // The writer's two increments are one critical section, so a reader sees both or
                // neither.
                assert_eq!(guard.0, guard.1, "a reader saw a half-written value");
            }
            writer.join().unwrap();
            assert_eq!(*lock.read().unwrap(), (1, 1));
        },
        None,
    );
}

#[test]
fn two_readers_hold_the_lock_at_once() {
    let both = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let both = Arc::clone(&both);
        check_random(
            move || {
                let lock = Arc::new(ShardedLock::new(0u32));
                let readers = Arc::new(AtomicUsize::new(0));
                let threads = (0..2)
                    .map(|_| {
                        let (lock, readers, both) = (Arc::clone(&lock), Arc::clone(&readers), Arc::clone(&both));
                        thread::spawn(move || {
                            let _guard = lock.read().unwrap();
                            if readers.fetch_add(1, Ordering::SeqCst) == 1 {
                                both.store(true, std::sync::atomic::Ordering::SeqCst);
                            }
                            thread::yield_now();
                            readers.fetch_sub(1, Ordering::SeqCst);
                        })
                    })
                    .collect::<Vec<_>>();
                for thread in threads {
                    thread.join().unwrap();
                }
            },
            50,
        );
    }
    assert!(
        both.load(std::sync::atomic::Ordering::SeqCst),
        "in no schedule did two readers hold the lock at the same time"
    );
}

#[test]
fn a_writer_waits_for_the_readers() {
    check_dfs(
        || {
            let lock = Arc::new(ShardedLock::new(0u32));
            let guard = lock.read().unwrap();
            let writer = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || *lock.write().unwrap() += 1)
            };
            // The writer cannot be in while this read guard is alive.
            assert_eq!(*guard, 0);
            drop(guard);
            writer.join().unwrap();
            assert_eq!(*lock.read().unwrap(), 1);
        },
        None,
    );
}

#[test]
fn a_panicking_writer_poisons_the_lock() {
    check_dfs(
        || {
            let lock = Arc::new(ShardedLock::new(0u32));
            let writer = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let _err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let _guard = lock.write().unwrap();
                        panic!("while holding the lock");
                    }))
                    .unwrap_err();
                })
            };
            writer.join().unwrap();
            assert!(lock.is_poisoned());
            assert!(lock.read().is_err());
            assert!(lock.try_write().is_err());
            // The value is still reachable through the error.
            assert_eq!(**lock.read().unwrap_err().get_ref(), 0);
        },
        None,
    );
}

// Bounded, as the `Condvar` under the wait group wakes every waiter and lets them race, which makes
// an exhaustive search of three tasks too large to finish.
#[test]
fn a_wait_group_waits_for_every_clone() {
    check_dfs_bounded(
        || {
            let wg = WaitGroup::new();
            let done = Arc::new(AtomicUsize::new(0));
            let workers = (0..2)
                .map(|_| {
                    let (wg, done) = (wg.clone(), Arc::clone(&done));
                    thread::spawn(move || {
                        done.fetch_add(1, Ordering::SeqCst);
                        drop(wg);
                    })
                })
                .collect::<Vec<_>>();
            wg.wait();
            // Every worker has run by the time the wait returns, in every schedule.
            assert_eq!(done.load(Ordering::SeqCst), 2);
            for worker in workers {
                worker.join().unwrap();
            }
        },
        None,
    );
}

// Every clone waiting at once: they all get past together, and the scheduler decides in which order.
#[test]
fn every_waiter_of_a_wait_group_gets_through() {
    let orders = random_outcomes(
        || {
            let wg = WaitGroup::new();
            let order = Arc::new(Mutex::new(Vec::new()));
            let others = [1u8, 2]
                .map(|id| {
                    let (wg, order) = (wg.clone(), Arc::clone(&order));
                    thread::spawn(move || {
                        wg.wait();
                        order.lock().unwrap().push(id);
                    })
                })
                .into_iter()
                .collect::<Vec<_>>();
            wg.wait();
            order.lock().unwrap().push(0);
            for other in others {
                other.join().unwrap();
            }
            order.lock().unwrap().clone()
        },
        200,
    );
    assert!(orders.len() > 1, "only one order was explored: {orders:?}");
    assert!(orders.iter().all(|order| order.len() == 3), "{orders:?}");
}

#[test]
#[should_panic(expected = "deadlock")]
fn a_wait_group_clone_that_is_never_dropped_is_a_deadlock() {
    check_random(
        || {
            let wg = WaitGroup::new();
            let _never_dropped = wg.clone();
            wg.wait();
        },
        1,
    );
}

#[test]
fn an_unpark_before_the_park_leaves_the_token() {
    check_dfs(
        || {
            let parker = Parker::new();
            let unparker = parker.unparker().clone();
            unparker.unpark();
            // The token is there, so this does not block.
            parker.park();
            // ... and it is gone now, so a second park with a timeout times out.
            parker.park_timeout(MS);
        },
        None,
    );
}

#[test]
fn a_parked_task_waits_for_its_unparker() {
    check_dfs(
        || {
            let parker = Parker::new();
            let unparker = parker.unparker().clone();
            let flag = Arc::new(AtomicBool::new(false));
            let worker = {
                let flag = Arc::clone(&flag);
                thread::spawn(move || {
                    flag.store(true, Ordering::SeqCst);
                    unparker.unpark();
                })
            };
            parker.park();
            // The unpark carries the worker's writes, in every schedule.
            assert!(flag.load(Ordering::SeqCst));
            worker.join().unwrap();
        },
        None,
    );
}

#[test]
#[should_panic(expected = "deadlock")]
fn a_park_nobody_unparks_is_a_deadlock() {
    check_random(
        || {
            let parker = Parker::new();
            parker.park();
        },
        1,
    );
}

// A park with a long timeout does not time out while the unparking task can still run.
#[test]
fn a_timed_park_does_not_time_out_while_a_task_can_run() {
    check_dfs(
        || {
            let parker = Parker::new();
            let unparker = parker.unparker().clone();
            let worker = thread::spawn(move || {
                thread::yield_now();
                unparker.unpark();
            });
            // `peek` rather than `Instant::now`, which draws from Shuttle's generator, which an
            // exhaustive search refuses.
            let start = peek();
            parker.park_timeout(HOUR);
            // The park ended with the token, not with the timeout: the clock did not jump an hour.
            assert!(peek() < start + HOUR);
            worker.join().unwrap();
        },
        None,
    );
}

#[test]
fn a_timed_park_times_out_when_nothing_can_run() {
    check_random(
        || {
            let parker = Parker::new();
            let start = Instant::now();
            parker.park_timeout(20 * MS);
            assert!(start.elapsed() >= 20 * MS);
            // A deadline in the past returns at once.
            let now = Instant::now();
            parker.park_deadline(now);
            assert!(Instant::now() >= now);
        },
        20,
    );
}

// The usual pattern: park with a timeout in a loop until a flag is set. It ends rather than being
// reported as a deadlock.
#[test]
fn a_polling_park_loop_ends() {
    check_dfs_bounded(
        || {
            let parker = Parker::new();
            let unparker = parker.unparker().clone();
            let stop = Arc::new(AtomicBool::new(false));
            let worker = {
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    stop.store(true, Ordering::SeqCst);
                    unparker.unpark();
                })
            };
            let mut rounds = 0;
            while !stop.load(Ordering::SeqCst) {
                parker.park_timeout(10 * MS);
                rounds += 1;
                assert!(rounds < 5, "the loop did not end");
            }
            worker.join().unwrap();
        },
        None,
    );
}

#[test]
fn a_parker_moved_to_another_task() {
    check_dfs(
        || {
            let parker = Parker::new();
            let unparker = parker.unparker().clone();
            // The `Parker` goes to the worker, which parks on it; the main task unparks.
            let worker = thread::spawn(move || parker.park());
            unparker.unpark();
            worker.join().unwrap();
        },
        None,
    );
}
