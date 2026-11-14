//! The queues under Shuttle: where the scheduling points are, which interleavings Shuttle explores,
//! that a queue's contents belong to one execution, and that schedules replay.

use shuttle::scheduler::RandomScheduler;
use shuttle::{Runner, check_dfs, check_random, check_uncontrolled_nondeterminism, current, thread};
use shuttle_crossbeam_queue_inner::{ArrayQueue, SegQueue};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Runs `f`, and returns what it returned and how many scheduling points it went through.
fn points<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let before = current::context_switches();
    let result = f();
    (result, current::context_switches() - before)
}

/// Runs `f` under DFS, and returns the set of values it recorded over all executions.
fn outcomes<T: Ord + Send + std::fmt::Debug + 'static>(f: impl Fn(&dyn Fn(T)) + Send + Sync + 'static) -> BTreeSet<T> {
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

#[test]
fn array_queue_scheduling_points() {
    check_dfs(
        || {
            let q = ArrayQueue::new(1);
            assert_eq!(points(|| q.push(1)), (Ok(()), 1));
            assert_eq!(points(|| q.push(2)), (Err(2), 1));
            assert_eq!(points(|| q.is_full()), (true, 1));
            assert_eq!(points(|| q.is_empty()), (false, 1));
            assert_eq!(points(|| q.len()), (1, 1));
            assert_eq!(points(|| q.force_push(3)), (Some(1), 1));
            assert_eq!(points(|| q.pop()), (Some(3), 1));
            assert_eq!(points(|| q.pop()), (None, 1));
            assert_eq!(points(|| q.capacity()), (1, 0));
            assert_eq!(points(|| format!("{q:?}")).1, 0);

            let mut q = q;
            assert_eq!(points(|| q.push_mut(4)), (Ok(()), 0));
            assert_eq!(points(|| q.pop_mut()), (Some(4), 0));
            q.push_mut(5).unwrap();
            assert_eq!(points(move || q.into_iter().collect::<Vec<_>>()), (vec![5], 0));

            let (q, constructing) = points(|| ArrayQueue::new(2));
            assert_eq!(constructing, 0);
            q.push(6).unwrap();
            assert_eq!(points(move || drop(q)).1, 0);
        },
        None,
    );
}

#[test]
fn seg_queue_scheduling_points() {
    check_dfs(
        || {
            let q = SegQueue::new();
            assert_eq!(points(|| q.push(1)), ((), 1));
            assert_eq!(points(|| q.is_empty()), (false, 1));
            assert_eq!(points(|| q.len()), (1, 1));
            assert_eq!(points(|| q.pop()), (Some(1), 1));
            assert_eq!(points(|| q.pop()), (None, 1));
            assert_eq!(points(|| format!("{q:?}")).1, 0);

            let mut q = q;
            assert_eq!(points(|| q.push_mut(2)), ((), 0));
            assert_eq!(points(|| q.pop_mut()), (Some(2), 0));
            q.push_mut(3);
            assert_eq!(points(move || q.into_iter().collect::<Vec<_>>()), (vec![3], 0));

            let (q, constructing) = points(SegQueue::<u8>::default);
            assert_eq!(constructing, 0);
            assert_eq!(points(move || drop(q)).1, 0);
        },
        None,
    );
}

/// Two threads push one value each: Shuttle explores both orders.
#[test]
fn concurrent_pushes_take_effect_in_either_order() {
    let orders = outcomes(|record| {
        let q = Arc::new(SegQueue::new());
        let pushers: Vec<_> = [1, 2]
            .into_iter()
            .map(|value| {
                let q = Arc::clone(&q);
                thread::spawn(move || q.push(value))
            })
            .collect();
        for pusher in pushers {
            pusher.join().unwrap();
        }
        record(std::iter::from_fn(|| q.pop()).collect::<Vec<_>>());
    });
    assert_eq!(orders, BTreeSet::from([vec![1, 2], vec![2, 1]]));
}

/// A pop that runs concurrently with a push sees the queue either before or after it, never in
/// between.
#[test]
fn a_pop_concurrent_with_a_push_is_linearizable() {
    let seen = outcomes(|record| {
        let q = Arc::new(ArrayQueue::new(2));
        q.push(0).unwrap();
        let pusher = {
            let q = Arc::clone(&q);
            thread::spawn(move || q.push(1).unwrap())
        };
        let first = q.pop();
        let second = q.pop();
        pusher.join().unwrap();
        let rest = q.pop();
        record((first, second, rest));
    });
    assert_eq!(
        seen,
        BTreeSet::from([(Some(0), None, Some(1)), (Some(0), Some(1), None)])
    );
}

/// With one free slot, two pushes race for it: exactly one wins, either can win, and the loser gets
/// its value back.
#[test]
fn two_pushes_race_for_the_last_slot() {
    let winners = outcomes(|record| {
        let q = Arc::new(ArrayQueue::new(1));
        let pushers: Vec<_> = [1, 2]
            .into_iter()
            .map(|value| {
                let q = Arc::clone(&q);
                thread::spawn(move || (value, q.push(value)))
            })
            .collect();
        let results: Vec<_> = pushers.into_iter().map(|p| p.join().unwrap()).collect();
        let won: Vec<_> = results.iter().filter(|(_, r)| r.is_ok()).map(|(v, _)| *v).collect();
        assert_eq!(won.len(), 1, "{results:?}");
        for (value, result) in &results {
            if let Err(back) = result {
                assert_eq!(back, value);
            }
        }
        assert_eq!(q.pop(), Some(won[0]));
        record(won[0]);
    });
    assert_eq!(winners, BTreeSet::from([1, 2]));
}

/// `force_push` on a full queue and a `pop` race: each value ends up in exactly one place, and both
/// orders happen.
#[test]
fn force_push_and_pop_race() {
    let seen = outcomes(|record| {
        let q = Arc::new(ArrayQueue::new(1));
        q.push(0).unwrap();
        let forcer = {
            let q = Arc::clone(&q);
            thread::spawn(move || q.force_push(1))
        };
        let popped = q.pop();
        let displaced = forcer.join().unwrap();
        let rest: Vec<_> = Arc::try_unwrap(q).unwrap().into_iter().collect();
        record((displaced, popped, rest));
    });
    assert_eq!(
        seen,
        BTreeSet::from([(None, Some(0), vec![1]), (Some(0), Some(1), vec![])])
    );
}

/// Several producers and consumers: every value is popped exactly once, and each producer's values
/// come out in the order it pushed them.
#[test]
fn no_value_is_lost_or_duplicated() {
    check_random(
        || {
            let q = Arc::new(SegQueue::new());
            let producers: Vec<_> = (0..3)
                .map(|producer| {
                    let q = Arc::clone(&q);
                    thread::spawn(move || {
                        for i in 0..3 {
                            q.push((producer, i));
                        }
                    })
                })
                .collect();
            let popped = Arc::new(AtomicUsize::new(0));
            let consumers: Vec<_> = (0..2)
                .map(|_| {
                    let q = Arc::clone(&q);
                    let popped = Arc::clone(&popped);
                    thread::spawn(move || {
                        let mut got = Vec::new();
                        while popped.load(Ordering::SeqCst) < 9 {
                            match q.pop() {
                                Some(value) => {
                                    popped.fetch_add(1, Ordering::SeqCst);
                                    got.push(value);
                                }
                                None => thread::yield_now(),
                            }
                        }
                        got
                    })
                })
                .collect();
            for producer in producers {
                producer.join().unwrap();
            }
            let mut all = Vec::new();
            for consumer in consumers {
                let got = consumer.join().unwrap();
                for producer in 0..3 {
                    let order: Vec<_> = got.iter().filter(|(p, _)| *p == producer).map(|(_, i)| *i).collect();
                    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{got:?}");
                }
                all.extend(got);
            }
            all.sort_unstable();
            let expected: Vec<_> = (0..3).flat_map(|p| (0..3).map(move |i| (p, i))).collect();
            assert_eq!(all, expected);
            assert!(q.is_empty());
        },
        200,
    );
}

static QUEUE: SegQueue<u32> = SegQueue::new();

/// A `static` queue starts every execution empty, however the execution before it left it.
#[test]
fn a_static_queue_starts_every_execution_empty() {
    let executions = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&executions);
    check_dfs(
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            assert!(QUEUE.is_empty());
            let pusher = thread::spawn(|| QUEUE.push(1));
            QUEUE.push(2);
            pusher.join().unwrap();
            assert_eq!(QUEUE.len(), 2);
        },
        None,
    );
    assert!(executions.load(Ordering::SeqCst) > 1);
}

/// So does a queue that the test closure captured.
#[test]
fn a_captured_queue_starts_every_execution_empty() {
    let bounded = Arc::new(ArrayQueue::new(1));
    let unbounded = Arc::new(SegQueue::new());
    check_random(
        move || {
            assert_eq!(bounded.push(1), Ok(()));
            assert_eq!(bounded.len(), 1);
            assert!(unbounded.is_empty());
            unbounded.push(1);
        },
        20,
    );
}

/// Values that a queue still holds when an execution ends are dropped with the execution; a queue
/// that is dropped during an execution drops them right away.
#[test]
fn values_are_dropped_with_their_queue_or_their_execution() {
    static DROPS: AtomicUsize = AtomicUsize::new(0);
    struct Counted;
    impl Drop for Counted {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }
    static LEFT: SegQueue<Counted> = SegQueue::new();

    check_random(|| LEFT.push(Counted), 10);
    assert_eq!(DROPS.swap(0, Ordering::SeqCst), 10);

    check_random(
        || {
            let q = ArrayQueue::new(2);
            q.push(Counted).ok().unwrap();
            let before = DROPS.load(Ordering::SeqCst);
            drop(q);
            assert_eq!(DROPS.load(Ordering::SeqCst), before + 1);
        },
        10,
    );
    assert_eq!(DROPS.load(Ordering::SeqCst), 10);
}

/// Three OS threads run Shuttle tests on the same `static` queue at once. Each execution sees only
/// its own values.
#[test]
fn a_static_queue_shared_by_concurrent_tests() {
    static SHARED: SegQueue<usize> = SegQueue::new();
    let tests: Vec<_> = (0..3)
        .map(|test| {
            std::thread::spawn(move || {
                check_random(
                    move || {
                        assert!(SHARED.is_empty());
                        SHARED.push(test);
                        thread::spawn(move || SHARED.push(test)).join().unwrap();
                        assert_eq!(SHARED.pop(), Some(test));
                        assert_eq!(SHARED.pop(), Some(test));
                        assert!(SHARED.pop().is_none());
                    },
                    50,
                )
            })
        })
        .collect();
    for test in tests {
        test.join().unwrap();
    }
}

/// The order in which three producers' values come out of a queue, in each of 30 executions with the
/// given seed.
fn pop_orders(seed: u64) -> Vec<Vec<usize>> {
    let log = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&log);
    let runner = Runner::new(RandomScheduler::new_from_seed(seed, 30), Default::default());
    runner.run(move || {
        let q = Arc::new(SegQueue::new());
        let producers: Vec<_> = (0..3)
            .map(|producer| {
                let q = Arc::clone(&q);
                thread::spawn(move || {
                    q.push(producer);
                    q.push(producer);
                })
            })
            .collect();
        let mut order = Vec::new();
        while order.len() < 6 {
            match q.pop() {
                Some(value) => order.push(value),
                None => thread::yield_now(),
            }
        }
        for producer in producers {
            producer.join().unwrap();
        }
        record.lock().unwrap().push(order);
    });
    Arc::try_unwrap(log).unwrap().into_inner().unwrap()
}

#[test]
fn the_same_seed_gives_the_same_interleavings() {
    let first = pop_orders(0x5EED);
    assert_eq!(first.len(), 30);
    assert_eq!(first, pop_orders(0x5EED));
    assert_ne!(first, pop_orders(0xD1FF));
}

/// A test whose schedule depends on a `static` queue's length.
fn branch_on_the_length(len: usize) {
    if len.is_multiple_of(2) {
        thread::spawn(|| ()).join().unwrap();
    }
}

#[test]
fn a_schedule_that_depends_on_a_static_queue_replays() {
    static MODEL: SegQueue<u32> = SegQueue::new();
    check_uncontrolled_nondeterminism(
        || {
            MODEL.push(1);
            branch_on_the_length(MODEL.len());
        },
        20,
    );
}

/// The control for the test above: crossbeam's own `SegQueue` in a `static` keeps its contents from
/// one execution to the next, so replaying a schedule takes another path.
#[test]
#[should_panic(expected = "possible nondeterminism")]
fn crossbeams_static_queue_does_not_replay() {
    static REAL: crossbeam_queue_orig::SegQueue<u32> = crossbeam_queue_orig::SegQueue::new();
    check_uncontrolled_nondeterminism(
        || {
            REAL.push(1);
            branch_on_the_length(REAL.len());
        },
        20,
    );
}

#[test]
#[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
fn an_operation_outside_of_shuttle_panics() {
    ArrayQueue::new(1).pop().unwrap_or(0u8);
}

#[test]
fn construction_capacity_and_drop_work_outside_of_shuttle() {
    let q = ArrayQueue::<String>::new(4);
    assert_eq!(q.capacity(), 4);
    assert_eq!(format!("{q:?}"), "ArrayQueue { .. }");
    assert_eq!(q.into_iter().count(), 0);
    drop(SegQueue::<String>::new());
}

#[test]
fn auto_traits_match_crossbeam() {
    fn send_sync<T: Send + Sync>() {}
    fn unwind_safe<T: std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}
    // Like crossbeam's, the queues are `Sync` for any `T: Send`.
    send_sync::<ArrayQueue<std::cell::Cell<u8>>>();
    send_sync::<SegQueue<std::cell::Cell<u8>>>();
    unwind_safe::<ArrayQueue<&mut u8>>();
    unwind_safe::<SegQueue<&mut u8>>();
}
