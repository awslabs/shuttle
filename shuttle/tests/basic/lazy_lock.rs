use shuttle::scheduler::{DfsScheduler, RandomScheduler};
use shuttle::sync::LazyLock;
use shuttle::thread::ThreadId;
use shuttle::{check_dfs, check_pct, check_uncontrolled_nondeterminism, current, thread, Runner};
use std::collections::{HashMap, HashSet};
use std::panic;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use test_log::test;

#[test]
fn basic() {
    static MAP: LazyLock<HashMap<u32, u32>> = LazyLock::new(|| {
        let mut map = HashMap::new();
        map.insert(1, 1);
        map
    });

    check_dfs(
        || {
            assert_eq!(LazyLock::get(&MAP), None);
            assert_eq!(MAP.get(&1), Some(&1));
            assert_eq!(LazyLock::get(&MAP).unwrap().get(&1), Some(&1));
        },
        None,
    );
}

#[test]
fn local() {
    check_dfs(
        || {
            let mut lazy = LazyLock::new(|| vec![1, 2, 3]);
            assert_eq!(LazyLock::get(&lazy), None);
            assert_eq!(LazyLock::force(&lazy), &vec![1, 2, 3]);
            assert_eq!(lazy.len(), 3);

            LazyLock::force_mut(&mut lazy).push(4);
            assert_eq!(LazyLock::get_mut(&mut lazy), Some(&mut vec![1, 2, 3, 4]));
            assert_eq!(LazyLock::into_inner(lazy).ok(), Some(vec![1, 2, 3, 4]));
        },
        None,
    );
}

#[test]
fn force_mut_initializes() {
    check_dfs(
        || {
            let mut lazy = LazyLock::new(|| 42);
            assert_eq!(LazyLock::get_mut(&mut lazy), None);
            *LazyLock::force_mut(&mut lazy) += 1;
            assert_eq!(*lazy, 43);

            // `into_inner` hands back the initializer of a lock that was never forced.
            let lazy = LazyLock::new(|| 42);
            let f = LazyLock::into_inner(lazy).unwrap_err();
            assert_eq!(f(), 42);
        },
        None,
    );
}

// A lock takes a scheduling point only where it may block: acquiring and releasing the lock that
// racing initializers contend on. A read of an initialized lock takes none.
#[test]
fn context_switches() {
    static LAZY: LazyLock<usize> = LazyLock::new(|| 42);

    check_dfs(
        || {
            assert_eq!(current::context_switches(), 1);

            // Reading an uninitialized lock does not block.
            assert_eq!(LazyLock::get(&LAZY), None);
            assert_eq!(current::context_switches(), 1);

            // Forcing it takes the lock: one scheduling point for the acquire, one for the
            // release.
            assert_eq!(*LAZY, 42);
            assert_eq!(current::context_switches(), 3);

            // Later reads do not block.
            assert_eq!(*LAZY, 42);
            assert_eq!(LazyLock::get(&LAZY), Some(&42));
            assert_eq!(current::context_switches(), 3);

            // Neither does initializing a lock through a `&mut` borrow, which cannot race.
            let mut lazy = LazyLock::new(|| 42);
            assert_eq!(*LazyLock::force_mut(&mut lazy), 42);
            assert_eq!(current::context_switches(), 3);
        },
        None,
    );
}

#[test]
fn default() {
    check_dfs(
        || {
            let lazy: LazyLock<Vec<usize>> = LazyLock::default();
            assert!(lazy.is_empty());
        },
        None,
    );
}

#[test]
fn deref_mut() {
    check_dfs(
        || {
            let mut lazy = LazyLock::new(|| vec![1]);
            lazy.push(2);
            assert_eq!(*lazy, vec![1, 2]);
        },
        None,
    );
}

// Run `num_threads` threads racing to initialize a lock and check that exactly one initializer
// ran, and that every thread won the race in some execution.
#[track_caller]
fn run_race_test(num_threads: usize, tester: impl FnOnce(Box<dyn Fn() + Send + Sync>)) {
    static INITIALIZERS: AtomicUsize = AtomicUsize::new(0);
    static WINNER: LazyLock<ThreadId> = LazyLock::new(|| {
        INITIALIZERS.fetch_add(1, Ordering::SeqCst);
        thread::current().id()
    });

    INITIALIZERS.store(0, Ordering::SeqCst);
    let winners = Arc::new(Mutex::new(HashSet::new()));
    let winners_clone = Arc::clone(&winners);

    tester(Box::new(move || {
        let threads = (0..num_threads).map(|_| thread::spawn(|| *WINNER)).collect::<Vec<_>>();

        let seen = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<HashSet<_>>();
        assert_eq!(seen.len(), 1, "every thread sees the same value");

        winners.lock().unwrap().insert(*WINNER);
    }));

    assert_eq!(
        winners_clone.lock().unwrap().len(),
        num_threads,
        "every thread should win the race in some execution"
    );
}

#[test]
fn racing_init_dfs() {
    run_race_test(2, |f| check_dfs(f, None));
}

#[test]
fn racing_init_pct() {
    run_race_test(4, |f| check_pct(f, 1000, 3));
}

// A `static` lock is uninitialized again in every execution, and its initializer runs once per
// execution.
#[test]
fn static_resets_across_executions() {
    static INITIALIZERS: AtomicUsize = AtomicUsize::new(0);
    static LAZY: LazyLock<usize> = LazyLock::new(|| {
        INITIALIZERS.fetch_add(1, Ordering::SeqCst);
        42
    });

    INITIALIZERS.store(0, Ordering::SeqCst);

    let runner = Runner::new(DfsScheduler::new(None, false), Default::default());
    let executions = runner.run(|| {
        // Nothing carried over from the execution before this one.
        assert_eq!(LazyLock::get(&LAZY), None);

        let threads = (0..2)
            .map(|_| thread::spawn(|| assert_eq!(*LAZY, 42)))
            .collect::<Vec<_>>();

        for thread in threads {
            thread.join().unwrap();
        }
    });

    assert!(executions > 1, "DFS should find more than one schedule");
    assert_eq!(
        INITIALIZERS.load(Ordering::SeqCst),
        executions,
        "the initializer should run exactly once per execution"
    );
}

// It is valid for an initializer to block, so other threads have to wait for it.
#[test]
fn init_with_yield() {
    static LAZY: LazyLock<usize> = LazyLock::new(|| {
        thread::yield_now();
        42
    });

    check_dfs(
        || {
            let thread = thread::spawn(|| assert_eq!(*LAZY, 42));
            assert_eq!(*LAZY, 42);
            thread.join().unwrap();
        },
        None,
    );
}

// A lock whose initializer panics is poisoned, and every later access to it panics, as `std`'s
// `LazyLock` does. Unlike `std`'s, the poisoning only lasts for this execution.
#[test]
fn poison() {
    static LAZY: LazyLock<usize> = LazyLock::new(|| panic!("expected panic"));

    check_dfs(
        || {
            let result = panic::catch_unwind(|| *LAZY);
            assert!(result.is_err(), "the panic should be propagated");

            // `get` does not panic on a poisoned lock, it reports no value.
            assert_eq!(LazyLock::get(&LAZY), None);

            let result = panic::catch_unwind(|| *LAZY);
            let message = *result.unwrap_err().downcast::<String>().unwrap();
            assert_eq!(message, "LazyLock instance has previously been poisoned");

            // So does an access from another thread.
            let thread = thread::spawn(|| {
                let result = panic::catch_unwind(|| LazyLock::force(&LAZY));
                let message = *result.unwrap_err().downcast::<String>().unwrap();
                assert_eq!(message, "LazyLock instance has previously been poisoned");
            });
            thread.join().unwrap();
        },
        None,
    );
}

// Poisoning is per execution: the panic in one execution does not carry into the next.
#[test]
fn poison_resets_across_executions() {
    static ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
    static LAZY: LazyLock<usize> = LazyLock::new(|| {
        ATTEMPTS.fetch_add(1, Ordering::SeqCst);
        panic!("expected panic");
    });

    ATTEMPTS.store(0, Ordering::SeqCst);

    let runner = Runner::new(DfsScheduler::new(None, false), Default::default());
    let executions = runner.run(|| {
        let result = panic::catch_unwind(|| *LAZY);
        assert!(result.is_err());
        // The initializer does not run again in this execution.
        let result = panic::catch_unwind(|| *LAZY);
        assert!(result.is_err());
    });

    assert_eq!(
        ATTEMPTS.load(Ordering::SeqCst),
        executions,
        "the initializer should run once in each execution, however it ended in the one before"
    );
}

// Forcing a lock from its own initializer deadlocks in `std`; Shuttle reports it.
#[test]
#[should_panic(expected = "deadlock")]
fn reentrant_force() {
    static LAZY: LazyLock<usize> = LazyLock::new(|| *LAZY);

    check_dfs(|| assert_eq!(*LAZY, 0), None);
}

// Replaying a seed reproduces the same sequence of events, which it cannot do if a lock keeps
// anything from one execution to the next.
#[test]
fn replay_from_seed() {
    static LAZY: LazyLock<ThreadId> = LazyLock::new(|| thread::current().id());

    fn run(seed: u64) -> Vec<(ThreadId, ThreadId)> {
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_clone = Arc::clone(&log);

        let runner = Runner::new(RandomScheduler::new_from_seed(seed, 50), Default::default());
        runner.run(move || {
            let threads = (0..3)
                .map(|_| {
                    let log = Arc::clone(&log);
                    thread::spawn(move || {
                        let winner = *LAZY;
                        log.lock().unwrap().push((thread::current().id(), winner));
                    })
                })
                .collect::<Vec<_>>();

            for thread in threads {
                thread.join().unwrap();
            }
        });

        Arc::try_unwrap(log_clone).unwrap().into_inner().unwrap()
    }

    let first = run(0x5EED);
    let second = run(0x5EED);
    assert!(!first.is_empty());
    assert_eq!(first, second);

    // A different seed explores different schedules, so the two runs should disagree somewhere.
    assert_ne!(first, run(0xD1FF));
}

#[test]
fn uncontrolled_nondeterminism() {
    static LAZY: LazyLock<ThreadId> = LazyLock::new(|| thread::current().id());

    check_uncontrolled_nondeterminism(
        || {
            let threads = (0..3).map(|_| thread::spawn(|| *LAZY)).collect::<Vec<_>>();

            let values = threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<HashSet<_>>();
            assert_eq!(values.len(), 1, "every thread sees the same value");
        },
        100,
    );
}

// Ensure that concurrent Shuttle tests see an isolated version of a `static` lock. This test is
// best effort, as it spawns OS threads and hopes they race.
#[test]
fn shared_static() {
    static INITIALIZERS: AtomicUsize = AtomicUsize::new(0);
    static LAZY: LazyLock<usize> = LazyLock::new(|| {
        INITIALIZERS.fetch_add(1, Ordering::SeqCst);
        42
    });

    INITIALIZERS.store(0, Ordering::SeqCst);
    let mut total_executions = 0;

    // Try a bunch of times to provoke the race
    for _ in 0..50 {
        let threads = (0..3)
            .map(|_| {
                std::thread::spawn(move || {
                    let runner = Runner::new(DfsScheduler::new(None, false), Default::default());
                    runner.run(|| {
                        let threads = (0..2)
                            .map(|_| thread::spawn(|| assert_eq!(*LAZY, 42)))
                            .collect::<Vec<_>>();

                        for thread in threads {
                            thread.join().unwrap();
                        }
                    })
                })
            })
            .collect::<Vec<_>>();

        total_executions += threads.into_iter().map(|handle| handle.join().unwrap()).sum::<usize>();
    }

    // The lock should be initialized exactly once per execution, otherwise the tests are
    // incorrectly sharing it
    assert_eq!(total_executions, INITIALIZERS.load(Ordering::SeqCst));
}
