//! The map and set under Shuttle: where the scheduling points are, which interleavings Shuttle
//! explores, that a map's entries belong to one execution, and that schedules replay.

use shuttle::scheduler::RandomScheduler;
use shuttle::{Runner, check_dfs, check_random, check_uncontrolled_nondeterminism, current, thread};
use shuttle_crossbeam_skiplist_inner::{SkipMap, SkipSet};
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

#[test]
fn scheduling_points() {
    check_dfs(
        || {
            let (map, constructing) = points(SkipMap::<i32, i32>::new);
            assert_eq!(constructing, 0);
            assert_eq!(points(|| map.is_empty()), (true, 1));
            assert_eq!(points(|| map.insert(1, 10).remove()).1, 1 + 1);
            assert_eq!(points(|| *map.insert(1, 11).value()), (11, 1));
            // Replacing takes two steps.
            assert_eq!(points(|| *map.insert(1, 12).value()), (12, 2));
            assert_eq!(points(|| *map.compare_insert(1, 13, |_| false).value()), (12, 1));
            assert_eq!(points(|| *map.compare_insert(1, 13, |_| true).value()), (13, 2));
            assert_eq!(points(|| *map.get_or_insert(1, 0).value()), (13, 1));
            assert_eq!(points(|| *map.get_or_insert(2, 20).value()), (20, 1));
            assert_eq!(points(|| *map.get_or_insert_with(2, || 0).value()), (20, 1));
            assert_eq!(points(|| *map.get_or_insert_with(3, || 30).value()), (30, 2));
            assert_eq!(points(|| map.len()), (3, 1));
            assert_eq!(points(|| map.contains_key(&2)), (true, 1));
            assert_eq!(points(|| map.get(&2).map(|e| *e.value())), (Some(20), 1));
            assert_eq!(points(|| map.front().map(|e| *e.key())), (Some(1), 1));
            assert_eq!(points(|| map.back().map(|e| *e.key())), (Some(3), 1));
            assert_eq!(
                points(|| map.lower_bound(std::ops::Bound::Excluded(&1)).map(|e| *e.key())),
                (Some(2), 1)
            );
            assert_eq!(
                points(|| map.upper_bound(std::ops::Bound::Excluded(&3)).map(|e| *e.key())),
                (Some(2), 1)
            );

            let (mut entry, _) = points(|| map.front().unwrap());
            assert_eq!(points(|| entry.key() + entry.value()), (14, 0));
            assert_eq!(points(|| entry.clone()).1, 0);
            assert_eq!(points(|| entry.is_removed()), (false, 1));
            assert_eq!(points(|| entry.next().map(|e| *e.key())), (Some(2), 1));
            assert_eq!(points(|| entry.prev().map(|e| *e.key())), (None, 1));
            assert_eq!(points(|| entry.move_next()), (true, 1));
            assert_eq!(points(|| entry.move_prev()), (true, 1));
            assert_eq!(points(|| format!("{entry:?}")).1, 0);
            assert_eq!(points(move || drop(entry)).1, 0);

            let (mut iter, making) = points(|| map.iter());
            assert_eq!(making, 0);
            assert_eq!(points(|| iter.next().map(|e| *e.key())), (Some(1), 1));
            assert_eq!(points(|| iter.next_back().map(|e| *e.key())), (Some(3), 1));
            let (mut range, making) = points(|| map.range(2..));
            assert_eq!(making, 0);
            assert_eq!(points(|| range.next().map(|e| *e.key())), (Some(2), 1));
            assert_eq!(points(|| range.next_back().map(|e| *e.key())), (Some(3), 1));
            drop((iter, range));

            assert_eq!(points(|| map.remove(&2).map(|e| *e.value())), (Some(20), 1));
            assert_eq!(points(|| map.pop_front().map(|e| *e.key())), (Some(1), 1));
            assert_eq!(points(|| map.pop_back().map(|e| *e.key())), (Some(3), 1));
            assert_eq!(points(|| map.pop_back().is_none()), (true, 1));

            // `clear`: a step per batch of 100, and a step per entry.
            for i in 0..150 {
                map.insert(i, i);
            }
            assert_eq!(points(|| map.clear()).1, 2 + 150);
            assert_eq!(points(|| map.clear()).1, 1);

            let (built, building) = points(|| (0..3).map(|i| (i, i)).collect::<SkipMap<_, _>>());
            assert_eq!(building, 0);
            assert_eq!(points(move || built.into_iter().count()), (3, 0));
            assert_eq!(points(move || drop(map)).1, 0);
        },
        None,
    );
}

/// Inserting over an existing key takes two steps, so a concurrent `get` sees the old value, the
/// new one, or, in between, no value.
#[test]
fn replacing_an_entry_leaves_a_moment_without_either() {
    let seen = outcomes(|record| {
        let map = Arc::new(SkipMap::new());
        map.insert("key", "old");
        let writer = {
            let map = Arc::clone(&map);
            thread::spawn(move || {
                map.insert("key", "new");
            })
        };
        let read = map.get("key").map(|entry| *entry.value());
        writer.join().unwrap();
        assert_eq!(*map.get("key").unwrap().value(), "new");
        record(read);
    });
    assert_eq!(seen, BTreeSet::from([None, Some("new"), Some("old")]));
}

/// Two threads insert the same key: the map ends up with one of the two values, and both orders
/// happen.
#[test]
fn concurrent_inserts_of_a_key() {
    let seen = outcomes(|record| {
        let map = Arc::new(SkipMap::new());
        let writers: Vec<_> = [1, 2]
            .into_iter()
            .map(|value| {
                let map = Arc::clone(&map);
                thread::spawn(move || {
                    map.insert("key", value);
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        assert_eq!(map.len(), 1);
        record(*map.get("key").unwrap().value());
    });
    assert_eq!(seen, BTreeSet::from([1, 2]));
}

/// `get_or_insert_with` runs its closure outside of the map, so another thread can insert the key
/// first: the closure ran, and its value is discarded.
#[test]
fn get_or_insert_with_can_lose_its_value() {
    let seen = outcomes(|record| {
        let map = Arc::new(SkipMap::new());
        let ran = Arc::new(AtomicUsize::new(0));
        let other = {
            let map = Arc::clone(&map);
            thread::spawn(move || {
                map.get_or_insert("key", "other");
            })
        };
        let value = *map
            .get_or_insert_with("key", || {
                ran.fetch_add(1, Ordering::SeqCst);
                "mine"
            })
            .value();
        other.join().unwrap();
        record((ran.load(Ordering::SeqCst), value));
    });
    assert_eq!(seen, BTreeSet::from([(0, "other"), (1, "mine"), (1, "other")]));
}

/// Two threads remove the same entry: exactly one of them did.
#[test]
fn two_removals_of_an_entry() {
    let seen = outcomes(|record| {
        let map = Arc::new(SkipMap::new());
        map.insert(1, ());
        let removers: Vec<_> = (0..2)
            .map(|_| {
                let map = Arc::clone(&map);
                thread::spawn(move || map.get(&1).map(|entry| entry.remove()))
            })
            .collect();
        let results: Vec<_> = removers.into_iter().map(|r| r.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| **r == Some(true)).count(), 1, "{results:?}");
        record(results);
    });
    assert!(seen.contains(&vec![Some(true), None]), "{seen:?}");
    assert!(seen.contains(&vec![Some(true), Some(false)]), "{seen:?}");
    assert!(seen.contains(&vec![Some(false), Some(true)]), "{seen:?}");
}

/// An iterator that runs while another thread removes and inserts sees keys in order, and sees
/// each key at most once. Whether it sees a key that is removed or inserted meanwhile depends on
/// where it is, and every combination happens.
#[test]
fn iterating_while_the_map_changes() {
    let seen = outcomes(|record| {
        let map = Arc::new(SkipMap::new());
        for key in [1, 3] {
            map.insert(key, ());
        }
        let writer = {
            let map = Arc::clone(&map);
            thread::spawn(move || {
                map.remove(&1);
                map.insert(2, ());
            })
        };
        let keys: Vec<i32> = map.iter().map(|entry| *entry.key()).collect();
        assert!(keys.windows(2).all(|pair| pair[0] < pair[1]), "{keys:?}");
        writer.join().unwrap();
        record(keys);
    });
    assert_eq!(seen, BTreeSet::from([vec![1, 2, 3], vec![1, 3], vec![2, 3], vec![3]]));
}

/// `clear` removes the entries one at a time, so an entry inserted behind it survives.
#[test]
fn clear_can_miss_an_entry_inserted_behind_it() {
    let seen = outcomes(|record| {
        let map = Arc::new(SkipMap::new());
        map.insert(2, ());
        map.insert(3, ());
        let inserter = {
            let map = Arc::clone(&map);
            thread::spawn(move || {
                map.insert(1, ());
            })
        };
        map.clear();
        inserter.join().unwrap();
        record(map.iter().map(|entry| *entry.key()).collect::<Vec<_>>());
    });
    assert_eq!(seen, BTreeSet::from([vec![], vec![1]]));
}

/// The closures that a map calls may use the same map: they run while its state is not borrowed.
#[test]
fn closures_may_use_the_map() {
    check_dfs(
        || {
            let map = SkipMap::new();
            map.insert(1, 1);
            let entry = map.compare_insert(1, 2, |old| map.contains_key(old) && map.len() == 1);
            assert_eq!(*entry.value(), 2);
            let entry = map.get_or_insert_with(5, || map.len() as i32);
            assert_eq!(*entry.value(), 1);
        },
        None,
    );
}

/// A map that the test closure captured starts every execution as it was built.
#[test]
fn a_captured_map_starts_every_execution_as_built() {
    let built: Arc<SkipMap<i32, String>> = Arc::new((0..3).map(|i| (i, i.to_string())).collect());
    let empty = Arc::new(SkipSet::new());
    let executions = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&executions);
    check_dfs(
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            assert_eq!(built.iter().map(|e| *e.key()).collect::<Vec<_>>(), vec![0, 1, 2]);
            let remover = {
                let built = Arc::clone(&built);
                thread::spawn(move || {
                    built.remove(&1);
                })
            };
            built.insert(7, "seven".into());
            remover.join().unwrap();
            assert_eq!(built.len(), 3);
            assert!(empty.is_empty());
            empty.insert(1);
        },
        None,
    );
    assert!(executions.load(Ordering::SeqCst) > 1);
}

/// Entries that a map still holds when an execution ends are dropped with the execution; a map that
/// is dropped during an execution drops them right away.
#[test]
fn entries_are_dropped_with_their_map_or_their_execution() {
    static DROPS: AtomicUsize = AtomicUsize::new(0);
    struct Counted;
    impl Drop for Counted {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }
    let captured = Arc::new(SkipMap::new());
    check_random(
        move || {
            captured.insert(1, Counted);
        },
        10,
    );
    assert_eq!(DROPS.swap(0, Ordering::SeqCst), 10);

    check_random(
        || {
            let map = SkipMap::new();
            map.insert(1, Counted);
            let before = DROPS.load(Ordering::SeqCst);
            drop(map);
            assert_eq!(DROPS.load(Ordering::SeqCst), before + 1);
        },
        10,
    );
    assert_eq!(DROPS.load(Ordering::SeqCst), 10);
}

/// What a reader saw of two writers' inserts, in each of 30 executions with the given seed.
fn reads(seed: u64) -> Vec<Vec<(i32, i32)>> {
    let log = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&log);
    let runner = Runner::new(RandomScheduler::new_from_seed(seed, 30), Default::default());
    runner.run(move || {
        let map = Arc::new(SkipMap::new());
        let writers: Vec<_> = (0..2)
            .map(|writer| {
                let map = Arc::clone(&map);
                thread::spawn(move || {
                    for key in 0..3 {
                        map.insert(key, writer);
                    }
                })
            })
            .collect();
        let read: Vec<(i32, i32)> = map.iter().map(|e| (*e.key(), *e.value())).collect();
        for writer in writers {
            writer.join().unwrap();
        }
        record.lock().unwrap().push(read);
    });
    Arc::try_unwrap(log).unwrap().into_inner().unwrap()
}

#[test]
fn the_same_seed_gives_the_same_reads() {
    let first = reads(0x5EED);
    assert_eq!(first.len(), 30);
    assert_eq!(first, reads(0x5EED));
    assert_ne!(first, reads(0xD1FF));
}

#[test]
fn a_schedule_that_depends_on_the_map_replays() {
    check_uncontrolled_nondeterminism(
        || {
            let map = Arc::new(SkipMap::new());
            let writer = {
                let map = Arc::clone(&map);
                thread::spawn(move || {
                    map.insert(1, ());
                })
            };
            if map.contains_key(&1) {
                thread::spawn(|| ()).join().unwrap();
            }
            writer.join().unwrap();
        },
        50,
    );
}

/// The control: crossbeam's own map, captured by the test closure, keeps its entries from one
/// execution to the next, so replaying a schedule takes another path.
#[test]
#[should_panic(expected = "possible nondeterminism")]
fn crossbeams_captured_map_does_not_replay() {
    let real = Arc::new(crossbeam_skiplist_orig::SkipMap::new());
    check_uncontrolled_nondeterminism(
        move || {
            let next = real.len();
            real.insert(next, ());
            if next.is_multiple_of(2) {
                thread::spawn(|| ()).join().unwrap();
            }
        },
        20,
    );
}

#[test]
#[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
fn an_operation_outside_of_shuttle_panics() {
    SkipSet::<u8>::new().insert(1);
}

#[test]
fn building_and_consuming_work_outside_of_shuttle() {
    let map: SkipMap<u8, u8> = [(2, 2), (1, 1), (2, 3)].into_iter().collect();
    let iter = map.iter();
    assert_eq!(format!("{iter:?}"), "Iter { .. }");
    drop(iter);
    assert_eq!(map.into_iter().collect::<Vec<_>>(), vec![(1, 1), (2, 2)]);
    let set: SkipSet<u8> = [3, 1].into_iter().collect();
    assert_eq!(set.into_iter().collect::<Vec<_>>(), vec![1, 3]);
}

#[test]
fn auto_traits_match_crossbeam() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<SkipMap<String, Vec<u8>>>();
    send_sync::<SkipSet<String>>();
    send_sync::<shuttle_crossbeam_skiplist_inner::map::Entry<'static, String, u8>>();
    send_sync::<shuttle_crossbeam_skiplist_inner::map::Iter<'static, String, u8>>();
}
