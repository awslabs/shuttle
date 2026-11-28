//! The pointers under Shuttle: where the scheduling points are, which interleavings Shuttle
//! explores, that a `static Atomic` starts every execution as constructed, that nothing is
//! reclaimed, and that schedules replay.

use shuttle::scheduler::RandomScheduler;
use shuttle::{Runner, check_dfs, check_random, check_uncontrolled_nondeterminism, current, thread};
use shuttle_crossbeam_epoch_inner::{self as epoch, Atomic, Collector, Owned, Shared};
use std::collections::BTreeSet;
use std::fmt::Debug;
use std::sync::atomic::Ordering::{Relaxed, SeqCst};
use std::sync::atomic::{AtomicUsize, Ordering as StdOrdering};
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

#[test]
fn atomic_scheduling_points() {
    check_dfs(
        || {
            let (a, constructing) = points(|| Atomic::new(1u64));
            assert_eq!(constructing, 0);
            assert_eq!(points(Atomic::<u64>::null).1, 0);
            assert_eq!(points(|| Owned::new(2u64)).1, 0);

            let guard = &epoch::pin();
            assert_eq!(points(|| a.load(SeqCst, guard)).1, 1);
            assert_eq!(points(|| a.load_consume(guard)).1, 1);
            assert_eq!(points(|| a.store(Owned::new(3u64), SeqCst)).1, 1);
            let old = points(|| a.swap(Owned::new(4u64), SeqCst, guard));
            assert_eq!(old.1, 1);
            // SAFETY: nothing else reaches the object that was swapped out.
            drop(unsafe { old.0.into_owned() });

            let current = a.load(SeqCst, guard);
            assert_eq!(
                points(|| a
                    .compare_exchange(current, Owned::new(5u64), SeqCst, SeqCst, guard)
                    .is_ok()),
                (true, 1)
            );
            let current = a.load(SeqCst, guard);
            assert_eq!(
                points(|| {
                    a.compare_exchange_weak(Shared::null(), Owned::new(6u64), SeqCst, SeqCst, guard)
                        .is_err()
                }),
                (true, 1)
            );
            // One load, then one attempt.
            assert_eq!(
                points(|| a.fetch_update(SeqCst, SeqCst, guard, |p| Some(p.with_tag(1))).is_ok()),
                (true, 2)
            );
            assert_eq!(points(|| a.fetch_and(0, SeqCst, guard).tag()), (1, 1));
            assert_eq!(points(|| a.fetch_or(0b11, SeqCst, guard).tag()), (0, 1));
            assert_eq!(points(|| a.fetch_xor(0b1, SeqCst, guard).tag()), (0b11, 1));
            assert_eq!(points(|| format!("{a:?}")).1, 1);
            assert_eq!(points(|| a.clone()).1, 1);

            // A `Shared` and an `Owned` keep no shared state.
            assert_eq!(points(|| current.tag() + current.with_tag(0).tag()).1, 0);
            assert_eq!(points(|| current.is_null()).1, 0);
            // SAFETY: nothing else reaches the object.
            assert_eq!(points(|| unsafe { *current.deref() }).1, 0);
            assert_eq!(points(|| Owned::new(7u64).with_tag(1).tag()), (1, 0));
            // SAFETY: nothing else reaches the object `a` points at.
            assert_eq!(points(move || unsafe { a.into_owned() }).1, 0);
        },
        None,
    );
}

#[test]
fn pinning_scheduling_points() {
    check_dfs(
        || {
            assert_eq!(points(epoch::is_pinned), (false, 0));
            let (guard, pinning) = points(epoch::pin);
            assert_eq!(pinning, 1);
            assert_eq!(points(epoch::is_pinned), (true, 0));
            // Reentrant pinning and unpinning touch nothing another task can see.
            assert_eq!(points(epoch::pin).1, 0);
            assert_eq!(points(|| guard.flush()).1, 1);
            let mut guard = guard;
            assert_eq!(points(|| guard.repin()).1, 1);
            assert_eq!(points(|| guard.repin_after(|| ())).1, 2);
            assert_eq!(points(|| guard.defer(|| ())).1, 0);
            assert_eq!(points(|| guard.collector().is_some()), (true, 0));
            assert_eq!(points(move || drop(guard)).1, 1);
            assert_eq!(points(epoch::is_pinned), (false, 0));

            // A custom collector's handle behaves the same way.
            let (collector, making) = points(Collector::new);
            assert_eq!(making, 0);
            let (handle, registering) = points(|| collector.register());
            assert_eq!(registering, 0);
            assert_eq!(points(|| handle.is_pinned()), (false, 0));
            let (guard, pinning) = points(|| handle.pin());
            assert_eq!(pinning, 1);
            assert_eq!(points(move || drop(guard)).1, 1);

            // The unprotected guard takes no scheduling point at all.
            // SAFETY: nothing else reaches the object below.
            unsafe {
                let unprotected = epoch::unprotected();
                assert_eq!(points(|| unprotected.flush()).1, 0);
                assert_eq!(points(|| unprotected.defer(|| ())).1, 0);
                assert_eq!(points(|| unprotected.collector().is_none()), (true, 0));
            }
        },
        None,
    );
}

/// Two threads swap into the same pointer: each sees the other's value or the original, every order
/// happens, and no value is lost.
#[test]
fn concurrent_swaps_are_linearizable() {
    let seen = outcomes(|record| {
        let a = Arc::new(Atomic::new(0u64));
        let swappers: Vec<_> = [1u64, 2]
            .into_iter()
            .map(|value| {
                let a = Arc::clone(&a);
                thread::spawn(move || {
                    let guard = &epoch::pin();
                    let old = a.swap(Owned::new(value), SeqCst, guard);
                    // SAFETY: a swapped-out object belongs to the task that swapped it out.
                    *unsafe { old.into_owned() }
                })
            })
            .collect();
        let taken: Vec<u64> = swappers.into_iter().map(|s| s.join().unwrap()).collect();
        let guard = &epoch::pin();
        let a = Arc::try_unwrap(a).ok().unwrap();
        // SAFETY: the swappers are done, and nothing else reaches the object.
        let left = *unsafe { a.load(SeqCst, guard).into_owned() };
        let mut all = taken;
        all.push(left);
        all.sort_unstable();
        assert_eq!(all, vec![0, 1, 2]);
        record(left);
    });
    assert_eq!(seen, BTreeSet::from([1, 2]));
}

/// Two compare-and-swaps from the same starting pointer race: exactly one wins, the loser gets its
/// value back, and both can win.
#[test]
fn two_compare_exchanges_race() {
    let winners = outcomes(|record| {
        let a = Arc::new(Atomic::new(0u64));
        // Both threads start from this pointer, which is a `usize` so it can cross threads.
        let initial: usize = <Shared<u64> as epoch::Pointer<u64>>::into_usize(a.load(SeqCst, &epoch::pin()));
        let racers: Vec<_> = [1u64, 2]
            .into_iter()
            .map(|value| {
                let a = Arc::clone(&a);
                thread::spawn(move || {
                    let guard = &epoch::pin();
                    // SAFETY: the word came from a load of `a` above; nothing frees its object.
                    let current = unsafe { <Shared<u64> as epoch::Pointer<u64>>::from_usize(initial) };
                    match a.compare_exchange(current, Owned::new(value), SeqCst, SeqCst, guard) {
                        Ok(_) => Some(value),
                        Err(err) => {
                            assert_eq!(*err.new, value);
                            None
                        }
                    }
                })
            })
            .collect();
        let results: Vec<_> = racers.into_iter().map(|r| r.join().unwrap()).collect();
        let won: Vec<u64> = results.into_iter().flatten().collect();
        assert_eq!(won.len(), 1);
        record(won[0]);
    });
    assert_eq!(winners, BTreeSet::from([1, 2]));
}

/// A Treiber stack built on the pointers: every value is pushed and popped exactly once, whatever
/// the schedule.
#[test]
fn a_treiber_stack_loses_nothing() {
    struct Node {
        value: u64,
        next: Atomic<Node>,
    }

    check_random(
        || {
            let head: Arc<Atomic<Node>> = Arc::new(Atomic::null());
            let pushers: Vec<_> = (1..=3)
                .map(|value| {
                    let head = Arc::clone(&head);
                    thread::spawn(move || {
                        let guard = &epoch::pin();
                        let mut node = Owned::new(Node {
                            value,
                            next: Atomic::null(),
                        });
                        loop {
                            let current = head.load(Relaxed, guard);
                            node.next.store(current, Relaxed);
                            match head.compare_exchange(current, node, SeqCst, Relaxed, guard) {
                                Ok(_) => break,
                                Err(err) => node = err.new,
                            }
                        }
                    })
                })
                .collect();
            let popper = {
                let head = Arc::clone(&head);
                thread::spawn(move || {
                    let guard = &epoch::pin();
                    let mut popped = Vec::new();
                    while popped.len() < 2 {
                        let current = head.load(SeqCst, guard);
                        // SAFETY: nothing is reclaimed under this model, so the node is alive.
                        match unsafe { current.as_ref() } {
                            None => thread::yield_now(),
                            Some(node) => {
                                let next = node.next.load(SeqCst, guard);
                                if head.compare_exchange(current, next, SeqCst, SeqCst, guard).is_ok() {
                                    popped.push(node.value);
                                    // The node is handed to the collector, which never frees it.
                                    // SAFETY: it is out of the stack, and no other task holds it.
                                    unsafe { guard.defer_destroy(current) };
                                }
                            }
                        }
                    }
                    popped
                })
            };
            for pusher in pushers {
                pusher.join().unwrap();
            }
            let popped = popper.join().unwrap();

            let guard = &epoch::pin();
            let mut left = Vec::new();
            let mut next = head.load(SeqCst, guard);
            // SAFETY: every task is done, and nothing is reclaimed.
            while let Some(node) = unsafe { next.as_ref() } {
                left.push(node.value);
                next = node.next.load(SeqCst, guard);
            }
            let mut all = popped;
            all.extend(left);
            all.sort_unstable();
            assert_eq!(all, vec![1, 2, 3]);
        },
        200,
    );
}

static HEAD: Atomic<u64> = Atomic::null();

/// A `static Atomic` starts every execution at its construction value, however the execution before
/// it left it.
#[test]
fn a_static_pointer_starts_every_execution_as_constructed() {
    let executions = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&executions);
    check_random(
        move || {
            counter.fetch_add(1, StdOrdering::SeqCst);
            let guard = &epoch::pin();
            assert!(HEAD.load(SeqCst, guard).is_null());
            let writer = thread::spawn(|| {
                let guard = &epoch::pin();
                HEAD.store(Owned::new(1u64), SeqCst);
                assert_eq!(unsafe { *HEAD.load(SeqCst, guard).deref() }, 1);
            });
            writer.join().unwrap();
            assert!(!HEAD.load(SeqCst, guard).is_null());
        },
        3,
    );
    assert!(executions.load(StdOrdering::SeqCst) > 1);
}

/// So does a pointer that the test closure captured.
#[test]
fn a_captured_pointer_starts_every_execution_as_constructed() {
    let zero = Arc::new(Atomic::new(0u64));
    check_random(
        move || {
            let guard = &epoch::pin();
            // SAFETY: nothing frees the object.
            assert_eq!(unsafe { *zero.load(SeqCst, guard).deref() }, 0);
            zero.store(Owned::new(1u64), SeqCst);
        },
        20,
    );
}

/// Nothing a deferred function would do happens: the object stays alive, its destructor does not
/// run, and the pointer to it can still be read in the same execution and in the next one.
#[test]
fn deferred_destruction_never_runs() {
    static DROPS: AtomicUsize = AtomicUsize::new(0);
    struct Counted(u64);
    impl Drop for Counted {
        fn drop(&mut self) {
            DROPS.fetch_add(1, StdOrdering::SeqCst);
        }
    }

    let leaked = Arc::new(Atomic::new(Counted(7)));
    let pointer = Arc::clone(&leaked);
    check_random(
        move || {
            let guard = &epoch::pin();
            let shared = pointer.load(SeqCst, guard);
            // SAFETY: nothing else reaches the object, which is what `defer_destroy` asks for.
            unsafe { guard.defer_destroy(shared) };
            guard.flush();
            // Still there, in this execution.
            // SAFETY: the model never reclaims.
            assert_eq!(unsafe { shared.deref() }.0, 7);
            // And a deferred function does not run either.
            guard.defer(|| DROPS.fetch_add(1000, StdOrdering::SeqCst));
        },
        20,
    );
    // Neither the destructor nor the function ran, in any of the 20 executions.
    assert_eq!(DROPS.load(StdOrdering::SeqCst), 0);
    // And the object is still there afterwards.
    // SAFETY: nothing freed it, and this takes it exactly once.
    let left = unsafe { Arc::try_unwrap(leaked).ok().unwrap().into_owned() };
    assert_eq!(left.0, 7);
    drop(left);
    assert_eq!(DROPS.load(StdOrdering::SeqCst), 1);
}

/// A function deferred on an unprotected guard runs immediately, as in crossbeam.
#[test]
fn an_unprotected_guard_defers_nothing() {
    static RAN: AtomicUsize = AtomicUsize::new(0);
    struct Counted;
    impl Drop for Counted {
        fn drop(&mut self) {
            RAN.fetch_add(1, StdOrdering::SeqCst);
        }
    }

    check_random(
        || {
            // SAFETY: nothing else reaches the object.
            unsafe {
                let guard = epoch::unprotected();
                let a = Atomic::new(Counted);
                let before = RAN.load(StdOrdering::SeqCst);
                guard.defer_destroy(a.load(SeqCst, guard));
                assert_eq!(RAN.load(StdOrdering::SeqCst), before + 1);
                guard.defer(|| RAN.fetch_add(1, StdOrdering::SeqCst));
                assert_eq!(RAN.load(StdOrdering::SeqCst), before + 2);
            }
        },
        5,
    );
    assert_eq!(RAN.load(StdOrdering::SeqCst), 10);
}

/// What a deferred closure captured is dropped when the execution ends, since the closure is.
#[test]
fn a_deferred_closure_is_dropped_with_the_execution() {
    static DROPS: AtomicUsize = AtomicUsize::new(0);
    struct Counted;
    impl Drop for Counted {
        fn drop(&mut self) {
            DROPS.fetch_add(1, StdOrdering::SeqCst);
        }
    }

    check_random(
        || {
            let owned = Owned::new(Counted);
            let guard = &epoch::pin();
            let before = DROPS.load(StdOrdering::SeqCst);
            // SAFETY: the closure owns what it touches.
            unsafe { guard.defer_unchecked(move || drop(owned)) };
            // Not yet: the closure is held until the execution ends.
            assert_eq!(DROPS.load(StdOrdering::SeqCst), before);
        },
        10,
    );
    assert_eq!(DROPS.load(StdOrdering::SeqCst), 10);
}

/// Each task gets its own participant in the default collector, and a fresh one in every execution.
#[test]
fn pinning_is_per_task_and_per_execution() {
    check_dfs(
        || {
            assert!(!epoch::is_pinned());
            let guard = epoch::pin();
            let other = thread::spawn(|| {
                // The other task is not pinned by ours.
                let pinned_before = epoch::is_pinned();
                let guard = epoch::pin();
                let pinned_now = epoch::is_pinned();
                drop(guard);
                (pinned_before, pinned_now, epoch::is_pinned())
            });
            assert_eq!(other.join().unwrap(), (false, true, false));
            assert!(epoch::is_pinned());
            drop(guard);
            assert!(!epoch::is_pinned());
            // Every task's handle belongs to the one default collector.
            assert_eq!(epoch::pin().collector(), Some(epoch::default_collector()));
        },
        None,
    );
}

/// Three OS threads run Shuttle tests on the same `static Atomic` at once. Each execution sees only
/// its own value.
#[test]
fn a_static_pointer_shared_by_concurrent_tests() {
    static SHARED: Atomic<usize> = Atomic::null();
    let tests: Vec<_> = (0..3)
        .map(|test| {
            std::thread::spawn(move || {
                check_random(
                    move || {
                        let guard = &epoch::pin();
                        assert!(SHARED.load(SeqCst, guard).is_null());
                        SHARED.store(Owned::new(test), SeqCst);
                        let shared = SHARED.load(SeqCst, guard);
                        // SAFETY: nothing frees the object, and each execution has its own word.
                        assert_eq!(unsafe { *shared.deref() }, test);
                        SHARED.store(Shared::null(), SeqCst);
                        // SAFETY: the pointer is out of the word, and no other task holds it.
                        drop(unsafe { shared.into_owned() });
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

/// The order in which two threads' compare-and-swaps land, in each of 30 executions with the given
/// seed.
fn swap_log(seed: u64) -> Vec<Vec<u64>> {
    let log = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&log);
    let runner = Runner::new(RandomScheduler::new_from_seed(seed, 30), Default::default());
    runner.run(move || {
        let a = Arc::new(Atomic::new(0u64));
        let swappers: Vec<_> = [1u64, 2]
            .into_iter()
            .map(|value| {
                let a = Arc::clone(&a);
                thread::spawn(move || {
                    let guard = &epoch::pin();
                    let old = a.swap(Owned::new(value), SeqCst, guard);
                    // SAFETY: a swapped-out object belongs to the task that swapped it out.
                    *unsafe { old.into_owned() }
                })
            })
            .collect();
        let order: Vec<u64> = swappers.into_iter().map(|s| s.join().unwrap()).collect();
        record.lock().unwrap().push(order);
    });
    Arc::try_unwrap(log).unwrap().into_inner().unwrap()
}

#[test]
fn the_same_seed_gives_the_same_swaps() {
    let first = swap_log(0x5EED);
    assert_eq!(first.len(), 30);
    assert_eq!(first, swap_log(0x5EED));
    assert_ne!(first, swap_log(0xD1FF));
}

/// A test whose schedule depends on what a load sees.
#[test]
fn a_schedule_that_depends_on_a_load_replays() {
    check_uncontrolled_nondeterminism(
        || {
            let a = Arc::new(Atomic::<u64>::null());
            let writer = {
                let a = Arc::clone(&a);
                thread::spawn(move || a.store(Owned::new(1), SeqCst))
            };
            let guard = &epoch::pin();
            if a.load(SeqCst, guard).is_null() {
                thread::spawn(|| ()).join().unwrap();
            }
            writer.join().unwrap();
            // SAFETY: the writer is done and nothing else reaches the object.
            drop(unsafe { Arc::try_unwrap(a).ok().unwrap().try_into_owned() });
        },
        50,
    );
}

/// The control: crossbeam's own `Atomic` in a `static` keeps its word from one execution to the
/// next, so replaying a schedule takes another path.
#[test]
#[should_panic(expected = "possible nondeterminism")]
fn crossbeams_static_pointer_does_not_replay() {
    static REAL: crossbeam_epoch_orig::Atomic<u64> = crossbeam_epoch_orig::Atomic::null();
    check_uncontrolled_nondeterminism(
        || {
            let guard = &crossbeam_epoch_orig::pin();
            let was_null = REAL.load(SeqCst, guard).is_null();
            REAL.store(crossbeam_epoch_orig::Owned::new(1), SeqCst);
            if was_null {
                thread::spawn(|| ()).join().unwrap();
            }
        },
        20,
    );
}

#[test]
#[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
fn a_load_outside_of_shuttle_panics() {
    // SAFETY: the unprotected guard protects nothing, and the load is what panics.
    unsafe {
        Atomic::new(1u64).load(SeqCst, epoch::unprotected());
    }
}

#[test]
#[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
fn pinning_outside_of_shuttle_panics() {
    let _ = epoch::pin();
}

#[test]
fn the_pointers_work_outside_of_shuttle() {
    let owned = Owned::new(1u64).with_tag(1);
    assert_eq!((*owned, owned.tag()), (1, 1));
    let atomic: Atomic<u64> = Atomic::from(owned);
    assert_eq!(format!("{:?}", Atomic::<u64>::null()), "Atomic { raw: 0, tag: 0 }");
    // SAFETY: nothing else reaches the object.
    let owned = unsafe { atomic.into_owned() };
    assert_eq!(*owned, 1);
    let collector = Collector::new();
    assert_eq!(*collector.register().collector(), collector);
}

#[test]
fn auto_traits_match_crossbeam() {
    fn send_sync<T: Send + Sync>() {}
    fn neither<T>() {}
    send_sync::<Atomic<u64>>();
    send_sync::<Collector>();
    // `Owned` is `Send + Sync` when its pointee is, as crossbeam's is; `Shared` is neither, since its
    // `PhantomData` includes `*const T`, as crossbeam's does.
    send_sync::<Owned<u64>>();
    // This would fail at compile time if `Shared` were `Send + Sync`: a trait with a `const IS_SYNC`
    // beats the blanket `impl<U: ?Sized> NotSync for U` when `U: Send + Sync`.
    trait NotSync {
        const IS_SYNC: bool = false;
    }
    impl<U: ?Sized> NotSync for U {}
    const _: () = assert!(!<Shared<'static, u64> as NotSync>::IS_SYNC);
    // A `Guard` and a `LocalHandle` belong to their task, as crossbeam's belong to their thread.
    neither::<epoch::Guard>();
    neither::<epoch::LocalHandle>();
    static_assertions();
}

fn static_assertions() {
    // These would fail at compile time if the types were `Send`, which is the point.
    trait NotSend {
        const IS_SEND: bool = false;
    }
    impl<T: ?Sized> NotSend for T {}
    struct Check<T: ?Sized>(std::marker::PhantomData<T>);
    impl<T: Send + ?Sized> Check<T> {
        #[allow(dead_code)]
        const IS_SEND: bool = true;
    }
    const _: () = {
        assert!(!<epoch::Guard as NotSend>::IS_SEND);
        assert!(!<epoch::LocalHandle as NotSend>::IS_SEND);
        assert!(<Check<Atomic<u64>>>::IS_SEND);
    };
}
