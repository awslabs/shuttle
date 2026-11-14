//! The scheduling points of each operation, counted with `current::context_switches()` in an
//! execution with a single task, where nothing blocks unless the test says so.

use shuttle::check_dfs;
use shuttle::current::context_switches;
use shuttle_crossbeam_utils_inner::atomic::{AtomicCell, AtomicConsume};
use shuttle_crossbeam_utils_inner::sync::{Parker, ShardedLock, WaitGroup};
use shuttle_crossbeam_utils_inner::{Backoff, CachePadded, thread as cb_thread};

/// The number of scheduling points `f` takes. What `f` returns is dropped after the count, so that
/// dropping it does not count.
fn points<R>(f: impl FnOnce() -> R) -> usize {
    let before = context_switches();
    let result = f();
    let points = context_switches() - before;
    drop(result);
    points
}

#[test]
fn backoff() {
    check_dfs(
        || {
            let backoff = Backoff::new();
            assert_eq!(points(Backoff::new), 0);
            assert_eq!(points(|| backoff.is_completed()), 0);
            assert_eq!(points(|| backoff.reset()), 0);
            // One point each, however far the backoff has got.
            for _ in 0..3 {
                assert_eq!(points(|| backoff.spin()), 1);
            }
            for _ in 0..3 {
                assert_eq!(points(|| backoff.snooze()), 1);
            }
            // Past the limits, where the real `spin` emits 2^6 hints and the real `snooze` yields
            // to the operating system.
            for _ in 0..12 {
                backoff.snooze();
            }
            assert!(backoff.is_completed());
            assert_eq!(points(|| backoff.snooze()), 1);
            assert_eq!(points(|| backoff.spin()), 1);
        },
        None,
    );
}

#[test]
fn atomic_cell() {
    check_dfs(
        || {
            assert_eq!(points(|| AtomicCell::new(1u32)), 0);
            let cell = AtomicCell::new(1u32);
            assert_eq!(points(|| cell.as_ptr()), 0);
            assert_eq!(points(AtomicCell::<u32>::is_lock_free), 0);
            assert_eq!(points(|| cell.load()), 1);
            assert_eq!(points(|| cell.store(2)), 1);
            assert_eq!(points(|| cell.swap(3)), 1);
            assert_eq!(points(|| cell.compare_exchange(3, 4)), 1);
            // A failed compare-exchange is one too.
            assert_eq!(points(|| cell.compare_exchange(99, 5)), 1);
            assert_eq!(points(|| cell.fetch_add(1)), 1);
            assert_eq!(points(|| cell.take()), 1);
            assert_eq!(points(|| format!("{cell:?}")), 1);
            // `fetch_update` is a load and then a compare-exchange, as crossbeam's is.
            assert_eq!(points(|| cell.fetch_update(|v| Some(v + 1))), 2);
            assert_eq!(points(|| cell.fetch_update(|_| None)), 1);
            assert_eq!(points(|| AtomicCell::new(1u32).into_inner()), 0);

            let flag = AtomicCell::new(true);
            assert_eq!(points(|| flag.fetch_or(false)), 1);

            // A cell too large for an atomic instruction is no different.
            let big = AtomicCell::new([0u8; 64]);
            assert_eq!(points(|| big.store([1u8; 64])), 1);
            assert_eq!(points(|| big.swap([2u8; 64])), 1);
        },
        None,
    );
}

#[test]
fn load_consume() {
    check_dfs(
        || {
            let value = shuttle::sync::atomic::AtomicUsize::new(1);
            // One, as a Shuttle atomic's `load` takes.
            assert_eq!(points(|| value.load_consume()), 1);
        },
        None,
    );
}

#[test]
fn sharded_lock() {
    check_dfs(
        || {
            assert_eq!(points(|| ShardedLock::new(1u32)), 0);
            let lock = ShardedLock::new(1u32);
            assert_eq!(points(|| lock.is_poisoned()), 0);
            // Taking a guard is one, and dropping it is one.
            let before = context_switches();
            let guard = lock.read().unwrap();
            assert_eq!(context_switches() - before, 1);
            assert_eq!(points(|| drop(guard)), 1);
            let before = context_switches();
            let guard = lock.write().unwrap();
            assert_eq!(context_switches() - before, 1);
            assert_eq!(points(|| drop(guard)), 1);
            // A `try_*` takes one too, whether it gets the lock or not, as Shuttle's `RwLock` has it.
            assert_eq!(points(|| lock.try_read().unwrap()), 1);
            assert_eq!(points(|| lock.try_write().unwrap()), 1);
            let guard = lock.try_read().unwrap();
            assert_eq!(points(|| lock.try_write().unwrap_err()), 1);
            assert_eq!(points(|| drop(guard)), 1);

            let mut lock = lock;
            assert_eq!(points(|| lock.get_mut().unwrap()), 0);
        },
        None,
    );
}

#[test]
fn cache_padded_has_nothing_to_schedule() {
    check_dfs(
        || {
            let padded = CachePadded::new(1u32);
            assert_eq!(points(|| *padded), 0);
            assert_eq!(points(|| CachePadded::new(2u32).into_inner()), 0);
        },
        None,
    );
}

#[test]
fn parker_and_wait_group() {
    check_dfs(
        || {
            assert_eq!(points(Parker::new), 0);
            let parker = Parker::new();
            assert_eq!(points(|| parker.unparker().clone()), 0);
            let unparker = parker.unparker().clone();
            // Making the token available is one, and taking it is one.
            assert_eq!(points(|| unparker.unpark()), 1);
            assert_eq!(points(|| parker.park()), 1);

            // The wait group's count is a Shuttle atomic, and the last reference also takes the
            // `Mutex` and notifies the `Condvar`.
            assert_eq!(points(WaitGroup::new), 0);
            let wg = WaitGroup::new();
            assert!(points(|| wg.clone()) >= 1);
            // The one reference left waits on nothing: its own drop releases the last one.
            assert!(points(|| wg.wait()) >= 1);
        },
        None,
    );
}

#[test]
fn spawning_a_scoped_thread_takes_one() {
    check_dfs(
        || {
            cb_thread::scope(|s| {
                let before = context_switches();
                let handle = s.spawn(|_| ());
                assert_eq!(context_switches() - before, 1);
                handle.join().unwrap();
            })
            .unwrap();
        },
        None,
    );
}
