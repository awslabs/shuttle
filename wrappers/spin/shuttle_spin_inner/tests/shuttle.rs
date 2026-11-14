//! How the model behaves across tasks: mutual exclusion, waiting instead of spinning, replay, and
//! the state of a `static` in each execution. Also what the real spin crate does under Shuttle,
//! where that is cheap to show.

use shuttle::sync::Arc;
use shuttle::sync::atomic::{AtomicUsize, Ordering};
use shuttle::{check_dfs, check_random, check_random_with_seed, check_uncontrolled_nondeterminism, thread};
use shuttle_spin_inner as spin;
use spin::mutex::{FairMutex, TicketMutex};
use spin::{Barrier, Lazy, Mutex, Once, RwLock};
use std::collections::HashSet;

/// Collects what each execution produced, for a check that runs executions.
fn collected<T: Send + 'static>(run: impl FnOnce(Arc<std::sync::Mutex<Vec<T>>>)) -> Vec<T> {
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    run(log.clone());
    std::mem::take(&mut *log.lock().unwrap())
}

#[test]
fn mutual_exclusion_dfs() {
    check_dfs(
        || {
            let mutex = Arc::new(Mutex::new(0u32));
            let threads: Vec<_> = (0..2)
                .map(|_| {
                    let mutex = mutex.clone();
                    thread::spawn(move || {
                        let mut guard = mutex.lock();
                        // A read, a yield and a write under the lock: if another task could take
                        // the lock in between, the count would come out below 2.
                        let value = *guard;
                        thread::yield_now();
                        *guard = value + 1;
                    })
                })
                .collect();
            for thread in threads {
                thread.join().unwrap();
            }
            assert_eq!(*mutex.lock(), 2);
        },
        None,
    );
}

#[test]
fn every_flavour_excludes() {
    macro_rules! exclusive {
        ($lock:expr) => {
            check_dfs(
                move || {
                    let lock = Arc::new($lock);
                    let threads: Vec<_> = (0..2)
                        .map(|_| {
                            let lock = lock.clone();
                            thread::spawn(move || {
                                let mut guard = lock.lock();
                                let value = *guard;
                                thread::yield_now();
                                *guard = value + 1;
                            })
                        })
                        .collect();
                    for thread in threads {
                        thread.join().unwrap();
                    }
                    assert_eq!(*lock.lock(), 2);
                },
                None,
            );
        };
    }
    exclusive!(spin::mutex::SpinMutex::<_>::new(0u32));
    exclusive!(TicketMutex::<_>::new(0u32));
    exclusive!(FairMutex::<_>::new(0u32));
}

/// A task that cannot take the lock waits for the holder instead of spinning. With the real spin
/// crate this execution never ends: the waiting task holds the only OS thread.
#[test]
fn a_waiting_task_lets_the_holder_run() {
    check_dfs(
        || {
            let mutex = Arc::new(Mutex::new(0u32));
            let guard = mutex.lock();
            let other = {
                let mutex = mutex.clone();
                thread::spawn(move || *mutex.lock())
            };
            // Let the other task reach the lock and block on it.
            thread::yield_now();
            drop(guard);
            assert_eq!(other.join().unwrap(), 0);
        },
        None,
    );
}

/// A ticket lock serves its waiters in the order in which they drew their tickets, and refuses a
/// `try_lock` by a newcomer while a waiter is ahead of it.
///
/// The draw order cannot be forced, because the draw is itself a scheduled step, so the test reads
/// it off the lock instead: once this task has released the lock, a `try_lock` fails exactly when
/// another task's ticket is ahead of it, and that task must then be served first. (A `try_lock`
/// that succeeds says nothing: the other task may have come and gone already.) Both outcomes occur
/// over the executions.
#[test]
fn ticket_lock_serves_in_draw_order() {
    let outcomes: HashSet<(bool, Vec<&'static str>)> = collected(|log| {
        check_random(
            move || {
                let mutex = Arc::new(TicketMutex::<_>::new(Vec::new()));
                let waiter = mutex.clone();
                let other = thread::spawn(move || waiter.lock().push("other"));
                let guard = mutex.lock();
                thread::yield_now();
                drop(guard);

                // Did the other task draw a ticket before we released the lock?
                let got_it = match mutex.try_lock() {
                    Some(mut guard) => {
                        guard.push("main");
                        true
                    }
                    None => false,
                };
                if !got_it {
                    mutex.lock().push("main");
                }
                other.join().unwrap();
                let order = mutex.lock().clone();
                log.lock().unwrap().push((got_it, order));
            },
            200,
        );
    })
    .into_iter()
    .collect();

    for (got_it, order) in &outcomes {
        assert_eq!(order.len(), 2, "each task took the lock once");
        if !*got_it {
            assert_eq!(
                order,
                &["other", "main"],
                "the task whose ticket was ahead was served first"
            );
        }
    }
    let try_lock_results: HashSet<bool> = outcomes.iter().map(|(got_it, _)| *got_it).collect();
    assert_eq!(
        try_lock_results,
        HashSet::from([true, false]),
        "the try_lock both succeeds and fails over the executions: {outcomes:?}"
    );
}

/// A reader and a writer exclude each other, and two readers do not.
#[test]
fn rwlock_readers_and_writers() {
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(0u32));
            let readers = Arc::new(AtomicUsize::new(0));
            let threads: Vec<_> = (0..2)
                .map(|i| {
                    let lock = lock.clone();
                    let readers = readers.clone();
                    thread::spawn(move || {
                        if i == 0 {
                            let mut guard = lock.write();
                            // No reader is in the lock while a writer is.
                            assert_eq!(readers.load(Ordering::SeqCst), 0);
                            *guard += 1;
                        } else {
                            let guard = lock.read();
                            readers.fetch_add(1, Ordering::SeqCst);
                            thread::yield_now();
                            readers.fetch_sub(1, Ordering::SeqCst);
                            assert!(*guard == 0 || *guard == 1);
                        }
                    })
                })
                .collect();
            for thread in threads {
                thread.join().unwrap();
            }
            assert_eq!(*lock.read(), 1);
        },
        None,
    );
}

/// Both orders of a reader and a writer are reachable: the test above would pass even if Shuttle
/// never ran the reader first.
#[test]
fn both_rwlock_orders_are_reachable() {
    let seen: HashSet<u32> = collected(|log| {
        check_random(
            move || {
                let lock = Arc::new(RwLock::new(0u32));
                let writer = {
                    let lock = lock.clone();
                    thread::spawn(move || *lock.write() += 1)
                };
                let read = *lock.read();
                writer.join().unwrap();
                log.lock().unwrap().push(read);
            },
            50,
        );
    })
    .into_iter()
    .collect();
    assert_eq!(
        seen,
        HashSet::from([0, 1]),
        "the reader runs before and after the writer"
    );
}

/// An upgradeable reader keeps a writer out while it decides, and its upgrade waits only for the
/// readers that are in the lock.
#[test]
fn upgrade_waits_for_readers_only() {
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(0u32));
            let upgradeable = lock.upgradeable_read();
            let writer = {
                let lock = lock.clone();
                thread::spawn(move || *lock.write() += 10)
            };
            thread::yield_now();
            // The writer cannot be in the lock: an upgradeable reader excludes it.
            let mut guard = upgradeable.upgrade();
            assert_eq!(*guard, 0, "the writer did not get in first");
            *guard += 1;
            drop(guard);
            writer.join().unwrap();
            assert_eq!(*lock.read(), 11);
        },
        None,
    );
}

#[test]
fn once_runs_one_initializer() {
    check_dfs(
        || {
            let once = Arc::new(Once::<u32>::new());
            let initializers = Arc::new(AtomicUsize::new(0));
            let threads: Vec<_> = (1..3)
                .map(|i| {
                    let once = once.clone();
                    let initializers = initializers.clone();
                    thread::spawn(move || {
                        *once.call_once(|| {
                            initializers.fetch_add(1, Ordering::SeqCst);
                            i
                        })
                    })
                })
                .collect();
            let values: Vec<_> = threads.into_iter().map(|thread| thread.join().unwrap()).collect();
            assert_eq!(initializers.load(Ordering::SeqCst), 1, "one initializer ran");
            assert_eq!(values[0], values[1], "both tasks see the same value");
            assert_eq!(once.get(), Some(&values[0]));
        },
        None,
    );
}

/// Over the executions, each task wins the initialization in some of them.
#[test]
fn every_task_can_win_the_once_race() {
    let winners: HashSet<u32> = collected(|log| {
        check_dfs(
            move || {
                let once = Arc::new(Once::<u32>::new());
                let threads: Vec<_> = (1..3)
                    .map(|i| {
                        let once = once.clone();
                        thread::spawn(move || *once.call_once(|| i))
                    })
                    .collect();
                for thread in threads {
                    thread.join().unwrap();
                }
                log.lock().unwrap().push(*once.get().unwrap());
            },
            None,
        );
    })
    .into_iter()
    .collect();
    assert_eq!(winners, HashSet::from([1, 2]));
}

/// A task that calls `wait` or `poll` while another initializes the `Once` blocks until the value
/// is there, rather than spinning.
#[test]
fn once_wait_blocks() {
    check_dfs(
        || {
            let once = Arc::new(Once::<u32>::new());
            let waiter = {
                let once = once.clone();
                thread::spawn(move || *once.wait())
            };
            let poller = {
                let once = once.clone();
                thread::spawn(move || once.poll().copied())
            };
            assert_eq!(*once.call_once(|| 7), 7);
            assert_eq!(waiter.join().unwrap(), 7);
            // `poll` returns `None` only if it ran before the initialization began.
            assert!(matches!(poller.join().unwrap(), None | Some(7)));
        },
        None,
    );
}

/// A `Barrier` releases its tasks only once all of them have arrived, and the leader is the last
/// to arrive, which over the executions is each of them.
#[test]
fn barrier_releases_together() {
    let leaders: HashSet<usize> = collected(|log| {
        check_dfs(
            move || {
                let barrier = Arc::new(Barrier::new(3));
                let arrived = Arc::new(AtomicUsize::new(0));
                let threads: Vec<_> = (0..3)
                    .map(|i| {
                        let barrier = barrier.clone();
                        let arrived = arrived.clone();
                        thread::spawn(move || {
                            arrived.fetch_add(1, Ordering::SeqCst);
                            let leader = barrier.wait().is_leader();
                            assert_eq!(arrived.load(Ordering::SeqCst), 3, "everyone arrived first");
                            (i, leader)
                        })
                    })
                    .collect();
                let results: Vec<_> = threads.into_iter().map(|thread| thread.join().unwrap()).collect();
                let leaders: Vec<_> = results.iter().filter(|(_, leader)| *leader).map(|(i, _)| *i).collect();
                assert_eq!(leaders.len(), 1, "exactly one leader");
                log.lock().unwrap().push(leaders[0]);
            },
            None,
        );
    })
    .into_iter()
    .collect();
    assert_eq!(leaders, HashSet::from([0, 1, 2]));
}

/// A `FairMutex`'s starving user gets the lock before a task that arrives later.
#[test]
fn starving_user_goes_first() {
    check_dfs(
        || {
            let mutex = Arc::new(FairMutex::<_>::new(Vec::new()));
            let guard = mutex.lock();
            let starving = mutex.starve();
            let newcomer = {
                let mutex = mutex.clone();
                thread::spawn(move || mutex.lock().push("newcomer"))
            };
            thread::yield_now();
            drop(guard);
            starving.lock().push("starving");
            newcomer.join().unwrap();
            assert_eq!(*mutex.lock(), ["starving", "newcomer"]);
        },
        None,
    );
}

// Statics, which every execution has to start afresh.
static STATIC_MUTEX: Mutex<u32> = Mutex::new(0);
static STATIC_RWLOCK: RwLock<u32> = RwLock::new(0);
static STATIC_TICKET: TicketMutex<u32> = TicketMutex::new(0);
static STATIC_FAIR: FairMutex<u32> = FairMutex::new(0);
static STATIC_ONCE: Once<u32> = Once::new();
static STATIC_LAZY: Lazy<u32> = Lazy::new(|| INITIALIZERS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) as u32);
static STATIC_BARRIER: Barrier = Barrier::new(2);
/// Counts the `Lazy` initializations across executions, so it is a real atomic.
static INITIALIZERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Counts the executions of `statics_reset_across_executions`, which a `'static` closure cannot
/// borrow a local for.
static EXECUTIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[test]
fn statics_reset_across_executions() {
    INITIALIZERS.store(0, std::sync::atomic::Ordering::SeqCst);
    check_random(
        || {
            EXECUTIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            // Every lock is unlocked, and holds the value it was created with.
            let mut guard = STATIC_MUTEX.lock();
            assert_eq!(*guard, 0);
            *guard += 1;
            let mut guard = STATIC_RWLOCK.write();
            assert_eq!(*guard, 0);
            *guard += 1;
            let mut guard = STATIC_TICKET.lock();
            assert_eq!(*guard, 0);
            *guard += 1;
            let mut guard = STATIC_FAIR.lock();
            assert_eq!(*guard, 0);
            *guard += 1;

            // The `Once` is empty, and the `Lazy` runs its initializer again.
            assert!(!STATIC_ONCE.is_completed());
            assert_eq!(*STATIC_ONCE.call_once(|| 1), 1);
            let lazy = *STATIC_LAZY;

            // The barrier has nobody waiting: a single task arriving second is the leader.
            let other = thread::spawn(|| STATIC_BARRIER.wait().is_leader());
            let leader = STATIC_BARRIER.wait().is_leader();
            assert_ne!(leader, other.join().unwrap(), "exactly one leader");

            // Each execution's `Lazy` value is that execution's initializer run.
            assert_eq!(
                u32::try_from(INITIALIZERS.load(std::sync::atomic::Ordering::SeqCst)).unwrap(),
                lazy + 1
            );
        },
        20,
    );
    assert_eq!(EXECUTIONS.load(std::sync::atomic::Ordering::SeqCst), 20);
    assert_eq!(
        INITIALIZERS.load(std::sync::atomic::Ordering::SeqCst),
        20,
        "the Lazy's initializer ran once per execution"
    );
}

/// Three OS threads running Shuttle tests on the same `static` `Once`, as upstream's `once.rs` and
/// `lazy_static.rs` tests do: one execution's state must not be another's.
#[test]
fn shared_static_once() {
    static SHARED_ONCE: Once<u32> = Once::new();
    let threads: Vec<_> = (0..3)
        .map(|_| std::thread::spawn(|| check_random(|| assert_eq!(*SHARED_ONCE.call_once(|| 1), 1), 30)))
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

/// Three OS threads running Shuttle tests on the same `static` lock, taking turns, as tests that
/// share a `static` lock have to (see the next test): each execution starts with the value the lock
/// was created with, which is copied into it.
#[test]
fn shared_static_lock_taken_in_turns() {
    static SHARED: Mutex<u32> = Mutex::new(5);
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let threads: Vec<_> = (0..3)
        .map(|_| {
            std::thread::spawn(|| {
                let _turn = TURN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                check_random(
                    || {
                        let mut guard = SHARED.lock();
                        assert_eq!(*guard, 5);
                        *guard += 1;
                    },
                    30,
                )
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

/// The message of a panic.
fn message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// A test that takes a `static` lock while a test running at the same time on another thread holds
/// it panics, naming that test's thread, as Shuttle's other locks do: two executions can't hold one
/// lock at once, even both to read it, and even if the value is copied into each execution, as a
/// lock can guard data outside of itself. For every kind of lock.
#[test]
fn a_static_lock_held_by_a_test_running_at_the_same_time_is_reported() {
    static MUTEX: Mutex<u32> = Mutex::new(0);
    static SPIN: spin::mutex::SpinMutex<Vec<u32>> = spin::mutex::SpinMutex::new(Vec::new());
    static TICKET: TicketMutex<u32> = TicketMutex::new(0);
    static FAIR: FairMutex<u32> = FairMutex::new(0);
    static RWLOCK: RwLock<u32> = RwLock::new(0);
    let (held, holding) = std::sync::mpsc::channel();
    let (done, is_done) = std::sync::mpsc::channel::<()>();
    let is_done = std::sync::Mutex::new(is_done);
    let holder = std::thread::Builder::new()
        .name("holder".into())
        .spawn(move || {
            check_random(
                move || {
                    let _mutex = MUTEX.lock();
                    let _spin = SPIN.lock();
                    let _ticket = TICKET.lock();
                    let _fair = FAIR.lock();
                    let _read = RWLOCK.read();
                    held.send(()).unwrap();
                    is_done.lock().unwrap().recv().unwrap();
                },
                1,
            )
        })
        .unwrap();
    holding.recv().unwrap();
    let tries: [fn(); 9] = [
        || drop(MUTEX.lock()),
        || drop(MUTEX.try_lock()),
        || drop(SPIN.lock()),
        || drop(TICKET.lock()),
        || drop(FAIR.lock()),
        || drop(RWLOCK.read()),
        || drop(RWLOCK.try_read()),
        || drop(RWLOCK.write()),
        || drop(RWLOCK.upgradeable_read()),
    ];
    for tried in tries {
        let result = std::thread::spawn(move || check_random(tried, 1)).join();
        let message = message(&*result.expect_err("a test that takes a lock that another test holds fails"));
        assert!(
            message.contains("a guard of the execution on thread 'holder', which runs at the same time"),
            "another panic: {message}"
        );
    }
    done.send(()).unwrap();
    holder.join().unwrap();
}

/// A guard that an execution forgot (`mem::forget`) holds the lock for good: the next execution
/// that takes the lock reports it, at once, as a guard left behind.
#[test]
fn a_forgotten_guard_is_reported_in_the_next_execution() {
    static FORGOTTEN: Mutex<u32> = Mutex::new(0);
    static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let result = std::panic::catch_unwind(|| {
        check_random(
            || {
                if RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    std::mem::forget(FORGOTTEN.lock());
                } else {
                    drop(FORGOTTEN.lock());
                }
            },
            2,
        )
    });
    let message = message(&*result.expect_err("the second execution reports the forgotten guard"));
    assert!(
        message.contains("a guard left behind by an execution that has ended"),
        "another panic: {message}"
    );
}

#[test]
fn same_seed_same_behaviour() {
    fn run(seed: u64) -> Vec<Vec<u32>> {
        collected(|log| {
            check_random_with_seed(
                move || {
                    let mutex = Arc::new(Mutex::new(Vec::new()));
                    let lock = Arc::new(RwLock::new(0u32));
                    let threads: Vec<_> = (0..3)
                        .map(|i| {
                            let mutex = mutex.clone();
                            let lock = lock.clone();
                            thread::spawn(move || {
                                mutex.lock().push(i);
                                if i == 0 {
                                    *lock.write() += 1;
                                } else {
                                    mutex.lock().push(*lock.read() + 10);
                                }
                            })
                        })
                        .collect();
                    for thread in threads {
                        thread.join().unwrap();
                    }
                    let order = mutex.lock().clone();
                    log.lock().unwrap().push(order);
                },
                seed,
                30,
            );
        })
    }
    let first = run(0x5EED);
    assert_eq!(first, run(0x5EED), "the same seed gives the same executions");
    assert_ne!(first, run(0xD1FF), "another seed gives others");
}

#[test]
fn nondeterminism_check_passes() {
    check_uncontrolled_nondeterminism(
        || {
            let mutex = Arc::new(Mutex::new(0u32));
            let lock = Arc::new(RwLock::new(0u32));
            let once = Arc::new(Once::<u32>::new());
            let threads: Vec<_> = (0..2)
                .map(|i| {
                    let mutex = mutex.clone();
                    let lock = lock.clone();
                    let once = once.clone();
                    thread::spawn(move || {
                        *mutex.lock() += 1;
                        if i == 0 {
                            *lock.write() += 1;
                        } else {
                            let _ = *lock.read();
                        }
                        once.call_once(|| i);
                    })
                })
                .collect();
            for thread in threads {
                thread.join().unwrap();
            }
        },
        50,
    );
}

/// What the real spin crate does under Shuttle, for comparison. The two differences that are cheap
/// to show without hanging the test are in here; the third, that a contended lock hangs the
/// execution, cannot be shown without hanging it.
mod real_spin {
    use super::*;

    /// The real crate's lock takes no scheduling point, so Shuttle never runs another task around
    /// it and misses the interleavings that the model explores (`both_lock_orders_are_reachable`
    /// in `scheduling_points.rs`).
    #[test]
    fn real_lock_is_not_a_scheduling_point() {
        check_dfs(
            || {
                let mutex = spin_orig::Mutex::new(0u32);
                let before = shuttle::current::context_switches();
                *mutex.lock() += 1;
                assert_eq!(shuttle::current::context_switches(), before);
            },
            None,
        );
    }

    /// The real crate's `Once` keeps its value in the `static` itself, so the first execution's
    /// value carries into every later one and only one initializer ever runs. A failing schedule
    /// of a later execution therefore cannot be replayed on its own.
    #[test]
    fn real_once_leaks_across_executions() {
        static REAL: spin_orig::Once<u32> = spin_orig::Once::new();
        static INITIALIZERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        static EXECUTIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        check_random(
            || {
                EXECUTIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                REAL.call_once(|| {
                    INITIALIZERS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    1
                });
            },
            10,
        );
        assert_eq!(EXECUTIONS.load(std::sync::atomic::Ordering::SeqCst), 10);
        assert_eq!(
            INITIALIZERS.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the real Once initialized once for all ten executions, where the model's does so in each"
        );
    }
}
