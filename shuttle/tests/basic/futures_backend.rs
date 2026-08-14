//! Tests for the futures backend, which polls each task as a future rather than running it on its
//! own stack. See `shuttle::TaskBackend::Futures`.

use shuttle::future::{self, batch_semaphore::BatchSemaphore};
use shuttle::sync::atomic::{AtomicUsize, Ordering};
use shuttle::sync::Arc;
use shuttle::{check_dfs_async, check_random_async};
use shuttle_engine::future::batch_semaphore::Fairness;
use shuttle_engine::runtime::execution::ExecutionState;
use std::collections::HashSet;
use std::sync::Mutex as StdMutex;
use test_log::test;

/// The simplest possible test: a body that awaits nothing at all.
#[test]
fn trivial() {
    check_dfs_async(|| async {}, None);
}

/// A spawned task's result reaches the join handle.
#[test]
fn spawn_and_join() {
    check_dfs_async(
        || async {
            let handle = future::spawn(async { 42u32 });
            assert_eq!(handle.await.unwrap(), 42);
        },
        None,
    );
}

/// Concurrent increments through an async mutex (a fair semaphore) must not lose updates.
#[test]
fn semaphore_mutual_exclusion() {
    check_random_async(
        || async {
            let counter = Arc::new(AtomicUsize::new(0));
            let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::StrictlyFair));

            let handles = (0..3)
                .map(|_| {
                    let counter = counter.clone();
                    let semaphore = semaphore.clone();
                    future::spawn(async move {
                        semaphore.acquire(1).await.unwrap();
                        // Critical section: a non-atomic read-modify-write, which would lose
                        // updates if the semaphore did not actually exclude.
                        let value = counter.load(Ordering::SeqCst);
                        future::yield_now().await;
                        counter.store(value + 1, Ordering::SeqCst);
                        semaphore.release(1);
                    })
                })
                .collect::<Vec<_>>();

            for handle in handles {
                handle.await.unwrap();
            }

            assert_eq!(counter.load(Ordering::SeqCst), 3);
        },
        200,
    );
}

/// The backend has to actually interleave tasks: two tasks appending to a shared log must be able
/// to produce more than one ordering.
#[test]
fn explores_interleavings() {
    let orderings = Arc::new(StdMutex::new(HashSet::new()));

    let recorded = orderings.clone();
    check_random_async(
        move || {
            let orderings = recorded.clone();
            async move {
                let log = Arc::new(StdMutex::new(Vec::new()));

                let handles = (0..2u32)
                    .map(|id| {
                        let log = log.clone();
                        future::spawn(async move {
                            for step in 0..2 {
                                log.lock().unwrap().push((id, step));
                                future::yield_now().await;
                            }
                        })
                    })
                    .collect::<Vec<_>>();

                for handle in handles {
                    handle.await.unwrap();
                }

                let log = log.lock().unwrap().clone();
                orderings.lock().unwrap().insert(log);
            }
        },
        100,
    );

    let orderings = orderings.lock().unwrap();
    assert!(
        orderings.len() > 1,
        "expected the scheduler to find multiple interleavings, saw {orderings:?}"
    );
}

/// A race that only shows up under some interleavings should be caught.
#[test]
#[should_panic(expected = "saw a torn update")]
fn finds_a_race() {
    check_random_async(
        || async {
            let counter = Arc::new(AtomicUsize::new(0));

            let handles = (0..2)
                .map(|_| {
                    let counter = counter.clone();
                    future::spawn(async move {
                        let value = counter.load(Ordering::SeqCst);
                        future::yield_now().await;
                        counter.store(value + 1, Ordering::SeqCst);
                    })
                })
                .collect::<Vec<_>>();

            for handle in handles {
                handle.await.unwrap();
            }

            assert_eq!(counter.load(Ordering::SeqCst), 2, "saw a torn update");
        },
        100,
    );
}

/// A blocking acquire is a genuine block: the task must be descheduled until permits arrive, and
/// the execution must still finish rather than deadlocking.
#[test]
fn blocking_acquire_unblocks_on_release() {
    check_dfs_async(
        || async {
            let semaphore = Arc::new(BatchSemaphore::new(0, Fairness::StrictlyFair));
            let done = Arc::new(AtomicUsize::new(0));

            let waiter = {
                let semaphore = semaphore.clone();
                let done = done.clone();
                future::spawn(async move {
                    semaphore.acquire(1).await.unwrap();
                    done.fetch_add(1, Ordering::SeqCst);
                })
            };

            semaphore.release(1);
            waiter.await.unwrap();
            assert_eq!(done.load(Ordering::SeqCst), 1);
        },
        None,
    );
}

/// Two tasks contending on a single permit, where each acquire blocks at least once.
#[test]
fn contended_semaphore_dfs() {
    check_dfs_async(
        || async {
            let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::StrictlyFair));
            let inside = Arc::new(AtomicUsize::new(0));

            let handles = (0..2)
                .map(|_| {
                    let semaphore = semaphore.clone();
                    let inside = inside.clone();
                    future::spawn(async move {
                        semaphore.acquire(1).await.unwrap();
                        assert_eq!(inside.fetch_add(1, Ordering::SeqCst), 0);
                        future::yield_now().await;
                        assert_eq!(inside.fetch_sub(1, Ordering::SeqCst), 1);
                        semaphore.release(1);
                    })
                })
                .collect::<Vec<_>>();

            for handle in handles {
                handle.await.unwrap();
            }
        },
        None,
    );
}

/// A deadlock is still detected on this backend.
#[test]
#[should_panic(expected = "deadlock")]
fn detects_deadlock() {
    check_dfs_async(
        || async {
            let semaphore = BatchSemaphore::new(0, Fairness::StrictlyFair);
            semaphore.acquire(1).await.unwrap();
        },
        None,
    );
}

/// Nested async functions suspend correctly: each poll has to re-enter through the whole chain and
/// land back at the innermost await point.
#[test]
fn deeply_nested_awaits() {
    async fn descend(depth: usize, counter: Arc<AtomicUsize>) {
        if depth == 0 {
            counter.fetch_add(1, Ordering::SeqCst);
            future::yield_now().await;
            counter.fetch_add(1, Ordering::SeqCst);
            return;
        }
        Box::pin(descend(depth - 1, counter)).await;
    }

    check_random_async(
        || async {
            let counter = Arc::new(AtomicUsize::new(0));
            let handles = (0..2)
                .map(|_| {
                    let counter = counter.clone();
                    future::spawn(descend(8, counter))
                })
                .collect::<Vec<_>>();
            for handle in handles {
                handle.await.unwrap();
            }
            assert_eq!(counter.load(Ordering::SeqCst), 4);
        },
        50,
    );
}

/// Synchronous blocking is unsupported on this backend and should say so clearly rather than
/// silently running a task that Shuttle believes is blocked.
#[test]
#[should_panic(expected = "the futures backend cannot support")]
fn synchronous_blocking_is_rejected() {
    check_dfs_async(
        || async {
            let lock = shuttle::sync::Mutex::new(0);
            let _guard = lock.lock().unwrap();
        },
        None,
    );
}

/// `thread::spawn` still gives you a task with its own stack, even inside an async test, because the
/// backend is chosen per task. That makes it an escape hatch for code that has to block
/// synchronously.
#[test]
fn spawned_threads_can_still_block() {
    check_random_async(
        || async {
            let lock = Arc::new(shuttle::sync::Mutex::new(0usize));
            let done = Arc::new(AtomicUsize::new(0));

            let _handles = (0..2)
                .map(|_| {
                    let lock = lock.clone();
                    let done = done.clone();
                    shuttle::thread::spawn(move || {
                        // A genuinely blocking lock, on a task that has its own stack.
                        *lock.lock().unwrap() += 1;
                        done.fetch_add(1, Ordering::SeqCst);
                    })
                })
                .collect::<Vec<_>>();

            // Wait for them by awaiting rather than joining, since this task cannot block.
            while done.load(Ordering::SeqCst) < 2 {
                future::yield_now().await;
            }

            assert_eq!(*lock.try_lock().unwrap(), 2);
        },
        50,
    );
}

/// A workload whose scheduling points are all `.await`s loses nothing on this backend.
#[test]
fn pure_await_workload_drops_no_scheduling_points() {
    check_dfs_async(
        || async {
            for _ in 0..5 {
                future::yield_now().await;
            }
            assert_eq!(
                ExecutionState::deferred_switches(),
                0,
                "a workload that only yields should not need to drop any scheduling points"
            );
        },
        None,
    );
}

/// Scheduling points requested from synchronous code get dropped, and are counted so that the
/// fidelity lost is visible rather than silent.
#[test]
fn synchronous_scheduling_points_are_counted() {
    check_dfs_async(
        || async {
            let counter = AtomicUsize::new(0);
            // Each atomic access wants a scheduling point, and none of them can have one.
            for _ in 0..5 {
                counter.fetch_add(1, Ordering::SeqCst);
            }
            assert!(
                ExecutionState::deferred_switches() >= 5,
                "expected the dropped scheduling points to be counted, got {}",
                ExecutionState::deferred_switches()
            );
        },
        Some(1),
    );
}

/// Reports how many scheduling steps each backend takes on the same workload. Run with
/// `cargo test --release -- --ignored --nocapture backend_step_comparison`.
///
/// This is the number to normalize benchmark timings by: the futures backend is faster per step,
/// but it also takes *fewer* steps, because scheduling points requested from synchronous code are
/// dropped. Only the first effect is a real efficiency win.
#[test]
#[ignore = "diagnostic, prints a report rather than asserting"]
fn backend_step_comparison() {
    use shuttle::scheduler::RandomScheduler;
    use shuttle::{Config, Runner, TaskBackend};
    use shuttle_engine::future::batch_semaphore::Fairness;

    fn measure(backend: TaskBackend, tasks: u32, acquires: u32) -> (usize, usize) {
        let stats = Arc::new(StdMutex::new((0usize, 0usize)));
        let recorded = stats.clone();
        let runner = Runner::new(
            RandomScheduler::new_from_seed(0x12345678, 1),
            Config::new().with_backend(backend),
        );
        runner.run_async(move || {
            let stats: Arc<StdMutex<(usize, usize)>> = recorded.clone();
            async move {
                let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::Unfair));
                let handles = (0..tasks)
                    .map(|_| {
                        let semaphore = semaphore.clone();
                        future::spawn(async move {
                            for _ in 0..acquires {
                                semaphore.acquire(1).await.unwrap();
                                semaphore.release(1);
                            }
                        })
                    })
                    .collect::<Vec<_>>();
                for handle in handles {
                    handle.await.unwrap();
                }
                *stats.lock().unwrap() = (
                    ExecutionState::context_switches(),
                    ExecutionState::deferred_switches(),
                );
            }
        });
        let result = *stats.lock().unwrap();
        result
    }

    fn measure_yields(backend: TaskBackend, tasks: u32, yields: u32) -> (usize, usize) {
        let stats = Arc::new(StdMutex::new((0usize, 0usize)));
        let recorded = stats.clone();
        let runner = Runner::new(
            RandomScheduler::new_from_seed(0x12345678, 1),
            Config::new().with_backend(backend),
        );
        runner.run_async(move || {
            let stats: Arc<StdMutex<(usize, usize)>> = recorded.clone();
            async move {
                let handles = (0..tasks)
                    .map(|_| {
                        future::spawn(async move {
                            for _ in 0..yields {
                                future::yield_now().await;
                            }
                        })
                    })
                    .collect::<Vec<_>>();
                for handle in handles {
                    handle.await.unwrap();
                }
                *stats.lock().unwrap() = (
                    ExecutionState::context_switches(),
                    ExecutionState::deferred_switches(),
                );
            }
        });
        let result = *stats.lock().unwrap();
        result
    }

    for (tasks, acquires) in [(4u32, 500u32), (32, 125)] {
        println!("\nsemaphore workload: {tasks} tasks x {acquires} acquires");
        for (name, backend) in [("stackful", TaskBackend::Stackful), ("futures", TaskBackend::Futures)] {
            let (steps, dropped) = measure(backend, tasks, acquires);
            println!("  {name:9} steps={steps:<8} dropped_scheduling_points={dropped}");
        }
    }

    for (tasks, yields) in [(4u32, 1000u32), (32, 250), (256, 32)] {
        println!("\nyield workload: {tasks} tasks x {yields} yields");
        for (name, backend) in [("stackful", TaskBackend::Stackful), ("futures", TaskBackend::Futures)] {
            let (steps, dropped) = measure_yields(backend, tasks, yields);
            println!("  {name:9} steps={steps:<8} dropped_scheduling_points={dropped}");
        }
    }
}
