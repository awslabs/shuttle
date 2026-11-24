use shuttle::check_dfs;
use shuttle_dashmap_impl::DashMap;

// An item of `iter` holds the read lock, also after the iterator is dropped.
#[test]
fn iter_item_holds_the_read_lock() {
    check_dfs(
        || {
            let map = DashMap::new();
            map.insert(1u32, 10u64);
            // The `Iter` is dropped at the end of this statement.
            let item = map.iter().next().unwrap();
            assert!(
                map.try_get_mut(&1).is_locked(),
                "the map can be written while an item of `iter()` lives"
            );
            assert_eq!(*item.value(), 10);
            drop(item);
            assert!(map.try_get_mut(&1).is_present());
        },
        None,
    );
}

// An item of `iter_mut` holds the write lock, also after the iterator is dropped.
#[test]
fn iter_mut_item_holds_the_write_lock() {
    check_dfs(
        || {
            let map = DashMap::new();
            map.insert(1u32, 10u64);
            // The `IterMut` is dropped at the end of this statement.
            let mut item = map.iter_mut().next().unwrap();
            assert!(
                map.try_get(&1).is_locked(),
                "the map can be read while an item of `iter_mut()` lives"
            );
            *item.value_mut() = 20;
            drop(item);
            assert_eq!(*map.try_get(&1).unwrap(), 20);
        },
        None,
    );
}

// A key whose `Clone` returns another key. `Clone` is a safe trait, so the map can't rely on a
// clone being equal to the original.
#[derive(Hash, PartialEq, Eq, Debug)]
struct Key(u32);

impl Clone for Key {
    fn clone(&self) -> Self {
        Key(0)
    }
}

// Each item of `iter_mut` points to a value of its own, also when the keys' `Clone` is wrong.
#[test]
fn iter_mut_items_point_to_different_values() {
    check_dfs(
        || {
            let map = DashMap::new();
            map.insert(Key(0), 0u64);
            map.insert(Key(1), 1u64);
            let mut it = map.iter_mut();
            let a = it.next().unwrap();
            let b = it.next().unwrap();
            assert!(
                !std::ptr::eq(a.value(), b.value()),
                "two items of `iter_mut()` point to the same value"
            );
            assert!(it.next().is_none());
        },
        None,
    );
}
