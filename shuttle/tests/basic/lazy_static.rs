use shuttle::scheduler::DfsScheduler;
use shuttle::thread::ThreadId;
use shuttle::{check_dfs, check_random, lazy_static, thread, Runner};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use test_log::test;

#[test]
fn basic() {
    shuttle::lazy_static! {
        static ref HASH_MAP: HashMap<u32, u32> = {
            let mut m = HashMap::new();
            m.insert(1, 1);
            m
        };
    }

    check_dfs(|| assert_eq!(HASH_MAP.get(&1), Some(&1)), None);
}

#[test]
fn racing_init() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    shuttle::lazy_static! {
        static ref HASH_MAP: HashMap<u32, u32> = {
            let mut m = HashMap::new();
            m.insert(1, 1);
            COUNTER.fetch_add(1, Ordering::SeqCst);
            m
        };
    }

    check_dfs(
        || {
            let thds = (0..2)
                .map(|_| {
                    thread::spawn(move || {
                        assert_eq!(HASH_MAP.get(&1), Some(&1));
                    })
                })
                .collect::<Vec<_>>();

            for thd in thds {
                thd.join().unwrap();
            }

            assert_eq!(COUNTER.swap(0, Ordering::SeqCst), 1);
        },
        None,
    );
}

// Same as `racing_init` but each thread first tries to manually initialize the static
#[test]
fn racing_init_explicit() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    shuttle::lazy_static! {
        static ref HASH_MAP: HashMap<u32, u32> = {
            let mut m = HashMap::new();
            m.insert(1, 1);
            COUNTER.fetch_add(1, Ordering::SeqCst);
            m
        };
    }

    check_dfs(
        || {
            let thds = (0..2)
                .map(|_| {
                    thread::spawn(move || {
                        lazy_static::initialize(&HASH_MAP);
                        assert_eq!(HASH_MAP.get(&1), Some(&1));
                    })
                })
                .collect::<Vec<_>>();

            for thd in thds {
                thd.join().unwrap();
            }

            assert_eq!(COUNTER.swap(0, Ordering::SeqCst), 1);
        },
        None,
    );
}

#[test]
fn init_with_yield() {
    shuttle::lazy_static! {
        static ref THING: Arc<usize> = {
            // check that it's valid to yield in the initializer
            thread::yield_now();
            Default::default()
        };
    }

    check_dfs(
        || {
            let thd = thread::spawn(|| {
                assert_eq!(**THING, 0);
            });

            assert_eq!(**THING, 0);

            thd.join().unwrap();
        },
        None,
    );
}

// Run N threads racing to acquire a Mutex and record the order they resolved the race in. Check
// that we saw all N! possible orderings to make sure `lazy_static` allows any thread to win the
// initialization race.
#[track_caller]
fn run_mutex_test(num_threads: usize, tester: impl FnOnce(Box<dyn Fn() + Send + Sync>)) {
    use std::sync::Mutex;

    shuttle::lazy_static! {
        static ref THREADS: Mutex<Vec<ThreadId>> = Mutex::new(Vec::new());
    }

    let initializers = Arc::new(Mutex::new(HashSet::new()));
    let initializers_clone = Arc::clone(&initializers);

    tester(Box::new(move || {
        let thds = (0..num_threads)
            .map(|_| {
                thread::spawn(|| {
                    THREADS.lock().unwrap().push(thread::current().id());
                })
            })
            .collect::<Vec<_>>();

        for thd in thds {
            thd.join().unwrap();
        }

        assert_eq!(THREADS.lock().unwrap().len(), num_threads);
        initializers.lock().unwrap().insert(THREADS.lock().unwrap().clone());
    }));

    let initializers = Arc::try_unwrap(initializers_clone).unwrap().into_inner().unwrap();
    assert_eq!(initializers.len(), (1..num_threads + 1).product::<usize>());
}

// Check all the interleavings of two racing threads trying to acquire a static Mutex.
#[test]
fn mutex_dfs() {
    run_mutex_test(2, |f| check_dfs(f, None));
}

// Like `mutex_dfs` but with more threads, making it too expensive to do DFS.
#[test]
fn mutex_random() {
    // Not guaranteed to pass, but should be pretty likely for 4 threads to see all 24 interleavings
    // in 10,000 iterations of a random scheduler
    run_mutex_test(4, |f| check_random(f, 10_000));
}

#[test]
fn chained() {
    shuttle::lazy_static! {
        static ref S1: Arc<usize> = Default::default();
        static ref S2: Arc<usize> = Arc::new(**S1);
    }

    check_dfs(
        move || {
            let thd = thread::spawn(|| {
                assert_eq!(**S2, 0);
            });

            assert_eq!(**S1, 0);

            thd.join().unwrap();
        },
        None,
    );
}

// Ensure that concurrent Shuttle tests see an isolated version of a lazy_static. This test is best
// effort, as it spawns OS threads and hopes they race.
#[test]
fn shared_static() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    shuttle::lazy_static! {
        static ref S: usize = {
            COUNTER.fetch_add(1, Ordering::SeqCst);
            0
        };
    }

    let mut total_executions = 0;

    // Try a bunch of times to provoke the race
    for _ in 0..50 {
        #[allow(clippy::needless_collect)] // https://github.com/rust-lang/rust-clippy/issues/7207
        let threads = (0..3)
            .map(|_| {
                std::thread::spawn(move || {
                    let scheduler = DfsScheduler::new(None, false);
                    let runner = Runner::new(scheduler, Default::default());
                    runner.run(move || {
                        let thds = (0..2)
                            .map(|_| {
                                thread::spawn(move || {
                                    assert_eq!(*S, 0);
                                })
                            })
                            .collect::<Vec<_>>();

                        for thd in thds {
                            thd.join().unwrap();
                        }
                    })
                })
            })
            .collect::<Vec<_>>();

        total_executions += threads.into_iter().map(|handle| handle.join().unwrap()).sum::<usize>();
    }

    // The static should be initialized exactly once per test execution, otherwise the tests are
    // incorrectly sharing state
    assert_eq!(total_executions, COUNTER.load(Ordering::SeqCst));
}

/// Check that the drop handler does not panic if executed while panicking.
#[test]
#[should_panic(expected = "the only panic")]
fn panic() {
    shuttle::lazy_static! {
        static ref S: usize = {
             42
        };
    }

    check_dfs(
        || {
            assert_eq!(*S, 42);
            panic!("the only panic");
        },
        None,
    );
}
