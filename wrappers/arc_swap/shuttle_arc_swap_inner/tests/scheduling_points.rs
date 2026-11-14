//! The scheduling points of each operation, counted in a single-task execution, where every
//! scheduling point has one choice and `context_switches` counts them all.

use shuttle::check_dfs;
use shuttle::current::context_switches;
use shuttle_arc_swap_inner::access::Access;
use shuttle_arc_swap_inner::{ArcSwap, ArcSwapOption, Cache, Guard};
use std::sync::Arc;

/// The scheduling points `operation` takes.
fn points(operation: impl Fn() + Send + Sync + 'static) -> usize {
    let count = Arc::new(std::sync::Mutex::new(0usize));
    let counted = count.clone();
    check_dfs(
        move || {
            let before = context_switches();
            operation();
            *counted.lock().unwrap() = context_switches() - before;
        },
        None,
    );
    *count.lock().unwrap()
}

#[test]
fn one_point_per_operation() {
    assert_eq!(points(|| drop(ArcSwap::from_pointee(1u32).load())), 1, "load");
    assert_eq!(points(|| drop(ArcSwap::from_pointee(1u32).load_full())), 1, "load_full");
    assert_eq!(points(|| ArcSwap::from_pointee(1u32).store(Arc::new(2))), 1, "store");
    assert_eq!(
        points(|| drop(ArcSwap::from_pointee(1u32).swap(Arc::new(2)))),
        1,
        "swap"
    );
    assert_eq!(
        points(|| {
            let storage = ArcSwap::from_pointee(1u32);
            let current = storage.load_full();
            // The load above is one, the exchange below is the second.
            drop(storage.compare_and_swap(&current, Arc::new(2)));
        }),
        2,
        "compare_and_swap that succeeds"
    );
    assert_eq!(
        points(|| {
            let storage = ArcSwap::from_pointee(1u32);
            drop(storage.compare_and_swap(Arc::as_ptr(&Arc::new(9u32)), Arc::new(2)));
        }),
        1,
        "compare_and_swap that fails"
    );
    assert_eq!(
        points(|| {
            let storage = ArcSwap::from_pointee(1u32);
            // `rcu` is a `load` and then a `compare_and_swap`, which succeeds the first time when
            // no other task is storing.
            drop(storage.rcu(|old| Arc::new(**old + 1)));
        }),
        2,
        "rcu, one attempt"
    );
    assert_eq!(
        points(|| drop(ArcSwap::from_pointee(1u32).into_inner())),
        0,
        "into_inner: the storage is ours alone"
    );
    assert_eq!(points(|| drop(ArcSwap::from_pointee(1u32))), 0, "construction and drop");
    assert_eq!(
        points(|| {
            let storage = ArcSwapOption::<u32>::const_empty();
            assert!(storage.load().is_none());
        }),
        1,
        "a const-constructed storage's first load, which also resets it"
    );
}

#[test]
fn accesses_and_caches_take_the_load_s_point() {
    assert_eq!(
        points(|| {
            let storage = ArcSwap::from_pointee(1u32);
            assert_eq!(*Access::<u32>::load(&storage), 1);
        }),
        1,
        "Access on the storage"
    );
    assert_eq!(
        points(|| {
            let storage = ArcSwap::from_pointee((1u32, 2u32));
            let projection = storage.map(|value: &(u32, u32)| &value.1);
            assert_eq!(*Access::load(&projection), 2);
        }),
        1,
        "a projection"
    );
    assert_eq!(
        points(|| {
            let storage = Arc::new(ArcSwap::from_pointee(1u32));
            // `Cache::new` loads once, and each `load` revalidates with another.
            let mut cache = Cache::new(storage);
            assert_eq!(**cache.load(), 1);
            assert_eq!(**cache.load(), 1);
        }),
        3,
        "Cache::new plus two loads"
    );
    assert_eq!(
        points(|| {
            let guard: Guard<Arc<u32>> = Guard::from_inner(Arc::new(1));
            assert_eq!(**guard, 1);
            assert_eq!(*Guard::into_inner(guard), 1);
        }),
        0,
        "a guard made by hand touches no storage"
    );
}
