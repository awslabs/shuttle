//! The work-stealing queues under Shuttle: where the scheduling points are, when a steal retries,
//! that a queue's tasks belong to one execution, and that schedules replay.

use shuttle::scheduler::RandomScheduler;
use shuttle::{Runner, check_dfs, check_random, check_uncontrolled_nondeterminism, current, thread};
use shuttle_crossbeam_deque_inner::{Injector, Steal, Stealer, Worker};
use std::collections::BTreeSet;
use std::fmt::Debug;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Runs `f`, and returns what it returned and how many scheduling points it went through.
fn points<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let before = current::context_switches();
    let result = f();
    (result, current::context_switches() - before)
}

/// Runs `f` under DFS, and returns the set of values it recorded over all executions.
fn outcomes<T: Ord + Send + Debug + 'static>(f: impl Fn(&dyn Fn(T)) + Send + Sync + 'static) -> BTreeSet<T> {
    let seen = Arc::new(Mutex::new(BTreeSet::new()));
    let record = Arc::clone(&seen);
    check_dfs(
        move || {
            f(&|outcome| {
                record.lock().unwrap().insert(outcome);
            })
        },
        None,
    );
    Arc::try_unwrap(seen).unwrap().into_inner().unwrap()
}

/// A worker with these tasks, pushed before anything else can see it.
fn filled(worker: Worker<u32>, tasks: std::ops::Range<u32>) -> Worker<u32> {
    for task in tasks {
        worker.push(task);
    }
    worker
}

#[test]
fn worker_and_stealer_scheduling_points() {
    check_dfs(
        || {
            let (w, constructing) = points(Worker::new_fifo);
            assert_eq!(constructing, 0);
            let (s, making) = points(|| w.stealer());
            assert_eq!(making, 0);
            assert_eq!(points(|| s.clone()).1, 0);
            assert_eq!(points(|| s.steal()), (Steal::Empty, 1));
            assert_eq!(points(|| w.push(1)), ((), 1));
            assert_eq!(points(|| w.len()), (1, 1));
            assert_eq!(points(|| w.is_empty()), (false, 1));
            assert_eq!(points(|| s.len()), (1, 1));
            assert_eq!(points(|| s.is_empty()), (false, 1));
            assert_eq!(points(|| s.steal()), (Steal::Success(1), 2));
            w.push(2);
            assert_eq!(points(|| w.pop()), (Some(2), 1));
            assert_eq!(points(|| w.pop()), (None, 1));
            assert_eq!(points(|| format!("{w:?} {s:?}")).1, 0);
            assert_eq!(points(move || drop((w, s))).1, 0);

            // Batch steals from a FIFO worker: one compare-and-swap.
            let source = filled(Worker::new_fifo(), 0..6);
            let dest = Worker::new_fifo();
            let s = source.stealer();
            assert_eq!(points(|| s.steal_batch(&dest)), (Steal::Success(()), 2));
            assert_eq!(points(|| s.steal_batch_and_pop(&dest)), (Steal::Success(3), 2));
            assert_eq!(points(|| s.steal_batch(&Worker::new_fifo())), (Steal::Success(()), 2));
            assert_eq!(points(|| s.steal_batch(&Worker::new_fifo())), (Steal::Empty, 1));
            assert_eq!(points(|| s.steal_batch_and_pop(&dest)), (Steal::Empty, 1));

            // From a LIFO worker: one step to look, and one per task it takes.
            let source = filled(Worker::new_lifo(), 0..6);
            let s = source.stealer();
            assert_eq!(points(|| s.steal_batch(&dest)), (Steal::Success(()), 1 + 3));
            // Three left: one extra task, and the one it pops.
            assert_eq!(points(|| s.steal_batch_and_pop(&dest)), (Steal::Success(4), 1 + 1 + 1));

            // Into its own worker: no stealing, just `is_empty` or `pop`.
            let own = filled(Worker::new_fifo(), 0..2);
            let s = own.stealer();
            assert_eq!(points(|| s.steal_batch(&own)), (Steal::Success(()), 1));
            assert_eq!(points(|| s.steal_batch_and_pop(&own)), (Steal::Success(0), 1));
        },
        None,
    );
}

#[test]
fn injector_scheduling_points() {
    check_dfs(
        || {
            let (q, constructing) = points(Injector::new);
            assert_eq!(constructing, 0);
            assert_eq!(points(|| q.steal()), (Steal::Empty, 1));
            assert_eq!(points(|| q.push(1)), ((), 1));
            assert_eq!(points(|| q.len()), (1, 1));
            assert_eq!(points(|| q.is_empty()), (false, 1));
            assert_eq!(points(|| q.steal()), (Steal::Success(1), 2));
            for i in 0..6 {
                q.push(i);
            }
            let w = Worker::new_lifo();
            assert_eq!(points(|| q.steal_batch(&w)), (Steal::Success(()), 2));
            assert_eq!(points(|| q.steal_batch_and_pop(&w)), (Steal::Success(3), 2));
            assert_eq!(
                points(|| q.steal_batch_with_limit_and_pop(&w, 1)),
                (Steal::Success(5), 2)
            );
            assert_eq!(points(|| q.steal_batch_with_limit(&w, 1)), (Steal::Empty, 1));
            assert_eq!(points(|| q.steal_batch_and_pop(&w)), (Steal::Empty, 1));
            assert_eq!(points(|| format!("{q:?}")).1, 0);
            assert_eq!(points(move || drop(q)).1, 0);
        },
        None,
    );
}

/// With nothing in between, a steal never retries: the model retries only when crossbeam would.
#[test]
fn a_steal_that_nothing_interferes_with_succeeds() {
    check_dfs(
        || {
            let w = filled(Worker::new_lifo(), 0..4);
            let s = w.stealer();
            let other = s.clone();
            // Another thread that does not touch the queue.
            let bystander = thread::spawn(thread::yield_now);
            assert_eq!(s.steal(), Steal::Success(0));
            assert_eq!(other.steal_batch(&Worker::new_fifo()), Steal::Success(()));
            assert_eq!(s.steal_batch_and_pop(&Worker::new_fifo()), Steal::Success(3));
            bystander.join().unwrap();
        },
        None,
    );
}

/// Two stealers race for one task. Exactly one gets it; the other finds the queue empty, or retries
/// if it looked before the winner took it. DFS reaches every outcome.
#[test]
fn two_steals_race_for_one_task() {
    let seen = outcomes(|record| {
        let w = filled(Worker::new_fifo(), 0..1);
        let thieves: Vec<_> = (0..2)
            .map(|_| {
                let s = w.stealer();
                thread::spawn(move || s.steal())
            })
            .collect();
        let results: Vec<_> = thieves.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_success()).count(), 1, "{results:?}");
        record(format!("{results:?}"));
    });
    assert_eq!(
        seen,
        BTreeSet::from(
            [
                "[Success(..), Empty]",
                "[Empty, Success(..)]",
                "[Success(..), Retry]",
                "[Retry, Success(..)]"
            ]
            .map(String::from)
        )
    );
}

/// A LIFO worker popping its last task races a stealer for it, as crossbeam's does: one of the two
/// gets it, and the stealer retries if the worker took it after the stealer looked.
#[test]
fn a_lifo_pop_of_the_last_task_races_a_steal() {
    let seen = outcomes(|record| {
        let w = filled(Worker::new_lifo(), 7..8);
        let s = w.stealer();
        let thief = thread::spawn(move || s.steal());
        let popped = w.pop();
        record((popped, format!("{:?}", thief.join().unwrap())));
    });
    assert_eq!(
        seen,
        BTreeSet::from([
            (Some(7), "Empty".to_string()),
            (Some(7), "Retry".to_string()),
            (None, "Success(..)".to_string()),
        ])
    );
}

/// A LIFO worker's pop of a task that is not the last one does not touch the front, so it never
/// makes a steal retry.
#[test]
fn a_lifo_pop_from_the_back_does_not_make_a_steal_retry() {
    let seen = outcomes(|record| {
        let w = filled(Worker::new_lifo(), 0..2);
        let s = w.stealer();
        let thief = thread::spawn(move || s.steal());
        let popped = w.pop();
        record((popped, thief.join().unwrap().success()));
    });
    assert_eq!(seen, BTreeSet::from([(Some(1), Some(0))]));
}

/// A FIFO worker pops from the front, so a steal that it overtakes retries.
#[test]
fn a_fifo_pop_makes_a_steal_retry() {
    let seen = outcomes(|record| {
        let w = filled(Worker::new_fifo(), 0..2);
        let s = w.stealer();
        let thief = thread::spawn(move || s.steal());
        let popped = w.pop();
        record((popped, format!("{:?}", thief.join().unwrap())));
    });
    assert_eq!(
        seen,
        BTreeSet::from([
            (Some(0), "Success(..)".to_string()),
            (Some(0), "Retry".to_string()),
            (Some(1), "Success(..)".to_string()),
        ])
    );
}

/// A push that grows crossbeam's buffer, from 64 tasks to 128, makes a steal in progress retry; a
/// push that fits does not.
#[test]
fn a_push_that_grows_the_buffer_makes_a_steal_retry() {
    for (tasks, retries) in [(64, true), (63, false)] {
        let seen = outcomes(move |record| {
            let w = filled(Worker::new_lifo(), 0..tasks);
            let s = w.stealer();
            let thief = thread::spawn(move || s.steal());
            w.push(tasks);
            record(thief.join().unwrap().is_retry());
        });
        let expected = if retries {
            BTreeSet::from([false, true])
        } else {
            BTreeSet::from([false])
        };
        assert_eq!(seen, expected, "with {tasks} tasks");
    }
}

/// Two steals from an injector race for its first task.
#[test]
fn two_injector_steals_race() {
    let seen = outcomes(|record| {
        let q = Arc::new(Injector::new());
        q.push(1);
        q.push(2);
        let thieves: Vec<_> = (0..2)
            .map(|_| {
                let q = Arc::clone(&q);
                thread::spawn(move || q.steal())
            })
            .collect();
        let results: Vec<_> = thieves.into_iter().map(|t| t.join().unwrap().success()).collect();
        record(results);
    });
    assert_eq!(
        seen,
        BTreeSet::from([
            vec![Some(1), Some(2)],
            vec![Some(2), Some(1)],
            vec![Some(1), None],
            vec![None, Some(1)],
        ])
    );
}

/// A push into an injector does not make a steal retry.
#[test]
fn an_injector_push_does_not_make_a_steal_retry() {
    let seen = outcomes(|record| {
        let q = Arc::new(Injector::new());
        q.push(1);
        let thief = {
            let q = Arc::clone(&q);
            thread::spawn(move || q.steal())
        };
        q.push(2);
        record(thief.join().unwrap().success());
    });
    assert_eq!(seen, BTreeSet::from([Some(1)]));
}

/// From a LIFO worker, a batch steal takes its tasks one at a time, so another steal can cut it
/// short: it takes two tasks, one, or retries.
#[test]
fn a_steal_can_cut_a_lifo_batch_short() {
    let seen = outcomes(|record| {
        let w = filled(Worker::new_lifo(), 0..4);
        let s = w.stealer();
        let other = w.stealer();
        let thief = thread::spawn(move || {
            let dest = Worker::new_fifo();
            let result = s.steal_batch(&dest);
            (format!("{result:?}"), dest.len())
        });
        let single = other.steal().is_success();
        let (batch, took) = thief.join().unwrap();
        record((batch, took, single));
    });
    assert!(seen.contains(&("Success(..)".to_string(), 2, true)), "{seen:?}");
    assert!(seen.contains(&("Success(..)".to_string(), 1, true)), "{seen:?}");
    assert!(seen.contains(&("Retry".to_string(), 0, true)), "{seen:?}");
    assert!(seen.iter().all(|(_, took, _)| *took <= 2), "{seen:?}");
}

/// A scheduler built on the queues runs every task exactly once, however the steals interleave and
/// retry. Retries do happen.
#[test]
fn every_task_runs_once() {
    static RETRIES: AtomicUsize = AtomicUsize::new(0);
    check_random(
        || {
            const TASKS: usize = 8;
            let global = Arc::new(Injector::new());
            for task in 0..TASKS {
                global.push(task);
            }
            let workers: Vec<Worker<usize>> = (0..3).map(|_| Worker::new_lifo()).collect();
            let stealers: Arc<Vec<Stealer<usize>>> = Arc::new(workers.iter().map(Worker::stealer).collect());
            let ran = Arc::new(AtomicUsize::new(0));
            let threads: Vec<_> = workers
                .into_iter()
                .map(|local| {
                    let global = Arc::clone(&global);
                    let stealers = Arc::clone(&stealers);
                    let ran = Arc::clone(&ran);
                    thread::spawn(move || {
                        let mut mine = Vec::new();
                        while ran.load(Ordering::SeqCst) < TASKS {
                            let task = local.pop().or_else(|| {
                                loop {
                                    let attempt = global
                                        .steal_batch_and_pop(&local)
                                        .or_else(|| stealers.iter().map(|s| s.steal()).collect());
                                    if attempt.is_retry() {
                                        RETRIES.fetch_add(1, Ordering::SeqCst);
                                    } else {
                                        break attempt.success();
                                    }
                                }
                            });
                            match task {
                                Some(task) => {
                                    mine.push(task);
                                    ran.fetch_add(1, Ordering::SeqCst);
                                }
                                None => thread::yield_now(),
                            }
                        }
                        mine
                    })
                })
                .collect();
            let mut all: Vec<usize> = threads.into_iter().flat_map(|t| t.join().unwrap()).collect();
            all.sort_unstable();
            assert_eq!(all, (0..TASKS).collect::<Vec<_>>());
        },
        300,
    );
    assert!(RETRIES.load(Ordering::SeqCst) > 0);
}

/// A queue that the test closure captured starts every execution empty.
#[test]
fn a_captured_queue_starts_every_execution_empty() {
    let injector = Arc::new(Injector::new());
    let worker = Worker::<u32>::new_fifo();
    let stealer = worker.stealer();
    drop(worker);
    check_random(
        move || {
            assert!(injector.is_empty());
            injector.push(1);
            assert!(stealer.is_empty());
            assert_eq!(stealer.steal(), Steal::Empty);
        },
        20,
    );
}

/// Tasks that a queue still holds when an execution ends are dropped with the execution; a queue
/// that is dropped during an execution drops them right away.
#[test]
fn tasks_are_dropped_with_their_queue_or_their_execution() {
    static DROPS: AtomicUsize = AtomicUsize::new(0);
    struct Counted;
    impl Drop for Counted {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }
    let left = Arc::new(Injector::new());
    check_random(move || left.push(Counted), 10);
    assert_eq!(DROPS.swap(0, Ordering::SeqCst), 10);

    check_random(
        || {
            let w = Worker::new_lifo();
            w.push(Counted);
            let s = w.stealer();
            drop(w);
            let before = DROPS.load(Ordering::SeqCst);
            drop(s);
            assert_eq!(DROPS.load(Ordering::SeqCst), before + 1);
        },
        10,
    );
    assert_eq!(DROPS.load(Ordering::SeqCst), 10);
}

/// What each of two stealers got, in each of 30 executions with the given seed.
fn steal_log(seed: u64) -> Vec<Vec<String>> {
    let log = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&log);
    let runner = Runner::new(RandomScheduler::new_from_seed(seed, 30), Default::default());
    runner.run(move || {
        let w = filled(Worker::new_fifo(), 0..3);
        let thieves: Vec<_> = (0..2)
            .map(|_| {
                let s = w.stealer();
                thread::spawn(move || (0..2).map(|_| format!("{:?}", s.steal().success())).collect::<Vec<_>>())
            })
            .collect();
        let popped = format!("{:?}", w.pop());
        let mut execution = vec![popped];
        for thief in thieves {
            execution.extend(thief.join().unwrap());
        }
        record.lock().unwrap().push(execution);
    });
    Arc::try_unwrap(log).unwrap().into_inner().unwrap()
}

#[test]
fn the_same_seed_gives_the_same_steals() {
    let first = steal_log(0x5EED);
    assert_eq!(first.len(), 30);
    assert_eq!(first, steal_log(0x5EED));
    assert_ne!(first, steal_log(0xD1FF));
}

/// A test whose schedule depends on whether a steal retried.
#[test]
fn a_schedule_that_depends_on_a_retry_replays() {
    check_uncontrolled_nondeterminism(
        || {
            let w = filled(Worker::new_fifo(), 0..1);
            let s = w.stealer();
            let thief = thread::spawn(move || s.steal());
            let _ = w.pop();
            if thief.join().unwrap().is_retry() {
                thread::spawn(|| ()).join().unwrap();
            }
        },
        50,
    );
}

/// The control: crossbeam's own injector, captured by the test closure, keeps its tasks from one
/// execution to the next, so replaying a schedule takes another path.
#[test]
#[should_panic(expected = "possible nondeterminism")]
fn crossbeams_captured_injector_does_not_replay() {
    let real = Arc::new(crossbeam_deque_orig::Injector::new());
    check_uncontrolled_nondeterminism(
        move || {
            real.push(1);
            if real.len().is_multiple_of(2) {
                thread::spawn(|| ()).join().unwrap();
            }
        },
        20,
    );
}

#[test]
#[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
fn an_operation_outside_of_shuttle_panics() {
    let _ = Worker::<u8>::new_fifo().stealer().steal();
}

#[test]
fn auto_traits_match_crossbeam() {
    fn send<T: Send>() {}
    fn send_sync<T: Send + Sync>() {}
    send::<Worker<std::cell::Cell<u8>>>();
    send_sync::<Stealer<std::cell::Cell<u8>>>();
    send_sync::<Injector<std::cell::Cell<u8>>>();
}
