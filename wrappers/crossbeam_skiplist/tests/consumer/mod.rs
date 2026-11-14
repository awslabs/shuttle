//! Code written against crossbeam-skiplist, as a crate that depends on `shuttle-crossbeam-skiplist`
//! under the name `crossbeam-skiplist` would have it. The same source is compiled against
//! `shuttle-crossbeam-skiplist` without the `shuttle` feature, where it is crossbeam-skiplist, and
//! against `shuttle-crossbeam-skiplist-inner`, which is what it is under `shuttle`. The test that
//! includes it provides `crate::thread`, std's threads or Shuttle's, and runs `exercise` under
//! Shuttle when the maps are Shuttle's.
//!
//! The single-threaded checks pin down what crossbeam-skiplist 0.1.3 does, so they also check that
//! the model does the same.

use crossbeam_skiplist::{SkipMap, SkipSet, map, set};
use std::ops::Bound::{Excluded, Included, Unbounded};
use std::sync::Arc;

/// Exercises the map's and the set's API.
pub fn exercise() {
    lookups();
    bounds();
    inserting();
    removing();
    entries_outlive_removal();
    cursors();
    iteration();
    iteration_sees_changes();
    ranges();
    clearing();
    building();
    sets();
    dropping();
    formatting();
    concurrent();
}

fn numbers() -> SkipMap<i32, &'static str> {
    let map = SkipMap::new();
    map.insert(6, "six");
    map.insert(7, "seven");
    map.insert(12, "twelve");
    map
}

fn lookups() {
    let map: SkipMap<&str, i32> = SkipMap::new();
    assert!(map.is_empty());
    assert!(map.get("six").is_none());
    map.insert("six", 6);
    assert_eq!(*map.get("six").unwrap().value(), 6);
    assert!(map.contains_key("six"));
    assert!(!map.contains_key("seven"));
    assert_eq!(map.len(), 1);

    let numbers = numbers();
    assert_eq!(*numbers.front().unwrap().value(), "six");
    assert_eq!(*numbers.back().unwrap().key(), 12);
}

fn bounds() {
    let numbers = numbers();
    assert_eq!(*numbers.lower_bound(Excluded(&5)).unwrap().value(), "six");
    assert_eq!(*numbers.lower_bound(Excluded(&6)).unwrap().value(), "seven");
    assert_eq!(*numbers.lower_bound(Included(&7)).unwrap().value(), "seven");
    assert!(numbers.lower_bound(Excluded(&13)).is_none());
    assert_eq!(*numbers.upper_bound(Excluded(&8)).unwrap().value(), "seven");
    assert_eq!(*numbers.upper_bound(Included(&12)).unwrap().value(), "twelve");
    assert!(numbers.upper_bound(Excluded(&6)).is_none());
    assert_eq!(*numbers.lower_bound(Unbounded).unwrap().key(), 6);
    assert_eq!(*numbers.upper_bound(Unbounded::<&i32>).unwrap().key(), 12);
}

fn inserting() {
    let ages = SkipMap::new();
    assert_eq!(*ages.get_or_insert("Bill Gates", 64).value(), 64);
    ages.insert("Steve Jobs", 65);
    assert_eq!(*ages.get_or_insert("Steve Jobs", -1).value(), 65);
    assert_eq!(*ages.get_or_insert_with("Steve Jobs", || -1).value(), 65);
    assert_eq!(*ages.get_or_insert_with("Ada", || 36).value(), 36);

    // `insert` replaces; the old entry is removed.
    let old = ages.get("Ada").unwrap();
    let new = ages.insert("Ada", 37);
    assert!(old.is_removed());
    assert!(!new.is_removed());
    assert_eq!((*old.value(), *new.value()), (36, 37));

    let map = SkipMap::new();
    map.insert("key", 1);
    map.compare_insert("key", 0, |x| x < &0);
    assert_eq!(*map.get("key").unwrap().value(), 1);
    map.compare_insert("key", 2, |x| x < &2);
    assert_eq!(*map.get("key").unwrap().value(), 2);
    map.compare_insert("absent_key", 0, |_| false);
    assert_eq!(*map.get("absent_key").unwrap().value(), 0);
}

fn removing() {
    let map: SkipMap<&str, &str> = SkipMap::new();
    assert!(map.remove("invalid key").is_none());
    map.insert("key", "value");
    assert_eq!(*map.remove("key").unwrap().value(), "value");
    assert!(map.is_empty());

    let numbers = numbers();
    assert_eq!(*numbers.pop_front().unwrap().value(), "six");
    assert_eq!(*numbers.pop_back().unwrap().value(), "twelve");
    assert_eq!(*numbers.pop_front().unwrap().value(), "seven");
    assert!(numbers.pop_front().is_none() && numbers.pop_back().is_none());
}

fn entries_outlive_removal() {
    let map = SkipMap::new();
    map.insert(1, String::from("one"));
    let entry = map.get(&1).unwrap();
    let clone = entry.clone();
    assert!(!entry.is_removed());
    assert!(entry.remove());
    // Only the call that removed it says so.
    assert!(!clone.remove());
    assert!(entry.is_removed() && clone.is_removed());
    assert_eq!(entry.value(), "one");
    assert!(map.get(&1).is_none());
}

fn cursors() {
    let map: SkipMap<i32, ()> = (0..5).map(|i| (i * 10, ())).collect();
    let mut entry = map.front().unwrap();
    assert_eq!(*entry.key(), 0);
    assert!(entry.move_next());
    assert_eq!(*entry.key(), 10);
    assert_eq!(*entry.next().unwrap().key(), 20);
    assert_eq!(*entry.prev().unwrap().key(), 0);
    assert!(entry.move_prev());
    assert!(!entry.move_prev());
    assert_eq!(*entry.key(), 0);

    // A removed entry still knows its neighbours, by key.
    let twenty = map.get(&20).unwrap();
    twenty.remove();
    assert_eq!(*twenty.next().unwrap().key(), 30);
    assert_eq!(*twenty.prev().unwrap().key(), 10);
    let mut last = map.back().unwrap();
    assert!(!last.move_next());
    assert_eq!(*last.key(), 40);
}

fn iteration() {
    let numbers = numbers();
    let forward: Vec<_> = numbers.iter().map(|e| *e.key()).collect();
    assert_eq!(forward, vec![6, 7, 12]);
    let backward: Vec<_> = numbers.iter().rev().map(|e| *e.key()).collect();
    assert_eq!(backward, vec![12, 7, 6]);
    let mut both = numbers.iter();
    assert_eq!(*both.next().unwrap().key(), 6);
    assert_eq!(*both.next_back().unwrap().key(), 12);
    assert_eq!(*both.next().unwrap().key(), 7);
    // The two ends have met.
    assert!(both.next().is_none() && both.next_back().is_none());
    let through_ref: Vec<_> = (&numbers).into_iter().map(|e| *e.value()).collect();
    assert_eq!(through_ref, vec!["six", "seven", "twelve"]);
}

/// Iterators are not snapshots: each step looks at the map as it is.
fn iteration_sees_changes() {
    let map: SkipMap<i32, i32> = (0..10).map(|i| (i, i)).collect();
    let mut iter = map.iter();
    assert_eq!(*iter.next().unwrap().key(), 0);
    // Removing entries while iterating, the next one included, is allowed.
    map.remove(&1);
    map.remove(&2);
    map.insert(20, 20);
    assert_eq!(*iter.next().unwrap().key(), 3);
    let rest: Vec<_> = iter.map(|e| *e.key()).collect();
    assert_eq!(rest, vec![4, 5, 6, 7, 8, 9, 20]);

    // Removing everything an iterator returns, as it goes.
    for entry in map.iter() {
        entry.remove();
    }
    assert!(map.is_empty());
}

fn ranges() {
    let numbers = numbers();
    let in_range: Vec<_> = numbers.range(5..=8).map(|e| *e.key()).collect();
    assert_eq!(in_range, vec![6, 7]);
    let open: Vec<_> = numbers.range(7..).map(|e| *e.key()).collect();
    assert_eq!(open, vec![7, 12]);
    let backward: Vec<_> = numbers.range(..12).rev().map(|e| *e.key()).collect();
    assert_eq!(backward, vec![7, 6]);
    let mut both = numbers.range(6..=12);
    assert_eq!(*both.next_back().unwrap().key(), 12);
    assert_eq!(*both.next().unwrap().key(), 6);
    assert_eq!(*both.next().unwrap().key(), 7);
    assert!(both.next().is_none() && both.next_back().is_none());
    assert_eq!(numbers.range(13..).count(), 0);

    // Borrowed keys.
    let words: SkipMap<String, usize> = ["apple", "banana", "cherry"]
        .iter()
        .map(|w| (w.to_string(), w.len()))
        .collect();
    let middle: Vec<_> = words
        .range::<str, _>((Excluded("apple"), Excluded("cherry")))
        .map(|e| *e.value())
        .collect();
    assert_eq!(middle, vec![6]);
}

fn clearing() {
    let people = SkipMap::new();
    people.insert("Bill", "Gates");
    people.insert("Steve", "Jobs");
    people.clear();
    assert!(people.is_empty());

    let many: SkipMap<u32, u32> = (0..250).map(|i| (i, i)).collect();
    many.clear();
    assert_eq!(many.len(), 0);
}

fn building() {
    // `from_iter` keeps the first value of a key.
    let map: SkipMap<i32, &str> = vec![(1, "first"), (2, "two"), (1, "second")].into_iter().collect();
    assert_eq!(map.len(), 2);
    assert_eq!(*map.get(&1).unwrap().value(), "first");
    let owned: Vec<(i32, &str)> = map.into_iter().collect();
    assert_eq!(owned, vec![(1, "first"), (2, "two")]);

    let default: SkipMap<u8, u8> = SkipMap::default();
    assert!(default.is_empty());
}

fn sets() {
    let set = SkipSet::new();
    assert!(set.is_empty());
    set.insert(3);
    set.insert(1);
    set.insert(2);
    assert_eq!(set.len(), 3);
    assert!(set.contains(&2));
    assert_eq!(*set.front().unwrap(), 1);
    assert_eq!(*set.back().unwrap().value(), 3);
    assert_eq!(*set.get(&2).unwrap(), 2);
    assert_eq!(*set.get_or_insert(2), 2);
    assert_eq!(*set.lower_bound(Excluded(&1)).unwrap(), 2);
    assert_eq!(*set.upper_bound(Excluded(&3)).unwrap(), 2);
    assert_eq!(set.iter().map(|e| *e).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(set.range(2..).map(|e| *e).collect::<Vec<_>>(), vec![2, 3]);
    assert_eq!(set.iter().rev().map(|e| *e).collect::<Vec<_>>(), vec![3, 2, 1]);

    let mut two = set.get(&2).unwrap();
    assert!(two.move_next());
    assert_eq!(*two, 3);
    assert_eq!(*two.prev().unwrap(), 2);
    assert!(two.remove());
    assert!(two.is_removed());
    assert_eq!(*set.pop_front().unwrap(), 1);
    assert_eq!(*set.pop_back().unwrap(), 2);
    assert!(set.remove(&3).is_none());

    let built: SkipSet<i32> = vec![5, 4, 5].into_iter().collect();
    assert_eq!(built.into_iter().collect::<Vec<_>>(), vec![4, 5]);
    let cleared: SkipSet<i32> = (0..10).collect();
    cleared.clear();
    assert!(cleared.is_empty());
    assert!(SkipSet::<u8>::default().is_empty());
}

fn dropping() {
    let token = Arc::new(());
    let map = SkipMap::new();
    map.insert(1, Arc::clone(&token));
    map.insert(2, Arc::clone(&token));
    let entry = map.remove(&1).unwrap();
    // A removed entry keeps its value alive for as long as an `Entry` holds it. When the value is
    // dropped after that is crossbeam's choice: it hands the node to its epoch-based collector.
    assert_eq!(Arc::strong_count(&token), 3);
    assert!(Arc::ptr_eq(entry.value(), &token));
    drop(entry);
    let after_removal = Arc::strong_count(&token);
    assert!(after_removal <= 3);

    // Dropping a map drops the values still in it, right away.
    drop(map);
    assert_eq!(Arc::strong_count(&token), after_removal - 1);
    let map = SkipMap::new();
    map.insert(1, Arc::clone(&token));
    map.insert(2, Arc::clone(&token));
    drop(map);
    assert_eq!(Arc::strong_count(&token), after_removal - 1);
}

fn formatting() {
    let map: SkipMap<i32, &str> = numbers();
    assert_eq!(format!("{map:?}"), "SkipMap { .. }");
    assert_eq!(format!("{:?}", map.get(&6).unwrap()), "Entry(6, \"six\")");
    assert_eq!(format!("{:?}", map.iter()), "Iter { .. }");
    let mut range = map.range(6..);
    assert_eq!(format!("{range:?}"), "Range { range: 6.., head: None, tail: None }");
    range.next();
    assert_eq!(
        format!("{range:?}"),
        "Range { range: 6.., head: Some(RefEntry(6, \"six\")), tail: None }"
    );
    assert_eq!(format!("{:?}", numbers().into_iter()), "IntoIter { .. }");

    let set: SkipSet<i32> = (1..4).collect();
    assert_eq!(format!("{set:?}"), "SkipSet { .. }");
    assert_eq!(format!("{:?}", set.front().unwrap()), "Entry { value: 1 }");
    let mut range = set.range(2..);
    range.next_back();
    assert_eq!(format!("{range:?}"), "Range { range: 2.., head: None, tail: Some(3) }");
    let _: &map::Entry<'_, i32, &str> = &map.front().unwrap();
    let _: set::Iter<'_, i32> = set.iter();
}

/// Two writers and a reader. Whatever the interleaving, each key ends up with one of the values
/// written to it, and the reader sees keys in order.
fn concurrent() {
    let map = Arc::new(SkipMap::new());
    let writers: Vec<_> = (0..2)
        .map(|writer| {
            let map = Arc::clone(&map);
            crate::thread::spawn(move || {
                for key in 0..3 {
                    map.insert(key, writer);
                }
                map.remove(&(writer + 10));
            })
        })
        .collect();
    let reader = {
        let map = Arc::clone(&map);
        crate::thread::spawn(move || {
            let keys: Vec<_> = map.iter().map(|e| *e.key()).collect();
            assert!(keys.windows(2).all(|pair| pair[0] < pair[1]), "{keys:?}");
        })
    };
    for writer in writers {
        writer.join().unwrap();
    }
    reader.join().unwrap();
    assert_eq!(map.len(), 3);
    for entry in map.iter() {
        assert!(*entry.value() < 2);
    }
}
