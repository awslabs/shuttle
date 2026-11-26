//! The scheduling points of each operation, counted in a single-task execution, where every
//! scheduling point has one choice and `context_switches` counts them all.

use shuttle::current::context_switches;
use shuttle::{check_dfs, check_random};
use shuttle_spin_inner as spin;
use spin::mutex::{FairMutex, SpinMutex, TicketMutex};
use spin::{Barrier, Lazy, Mutex, Once, RwLock};

/// The scheduling points `operation` takes.
fn points(operation: impl Fn() + Send + Sync + 'static) -> usize {
    let count = std::sync::Arc::new(std::sync::Mutex::new(0usize));
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
fn mutex_points() {
    assert_eq!(points(|| drop(Mutex::new(0).lock())), 2, "lock, release");
    assert_eq!(points(|| drop(Mutex::new(0).try_lock())), 2, "try_lock, release");
    assert_eq!(
        points(|| {
            let mutex = Mutex::new(0);
            let _guard = mutex.lock();
            assert!(mutex.try_lock().is_none());
        }),
        3,
        "lock, failed try_lock, release"
    );
    assert_eq!(points(|| assert!(!Mutex::new(0).is_locked())), 1, "is_locked");
    assert_eq!(
        points(|| {
            let mutex = Mutex::new(0);
            let guard = mutex.lock();
            let _ = spin::MutexGuard::leak(guard);
            // Safety: the guard was leaked.
            unsafe { mutex.force_unlock() };
        }),
        2,
        "lock, force_unlock; leaking takes none"
    );
    // The operations that need no synchronization take none, as Shuttle's `OnceLock` does.
    assert_eq!(points(|| assert_eq!(*Mutex::new(7).get_mut(), 7)), 0, "get_mut");
    assert_eq!(points(|| assert_eq!(Mutex::new(7).into_inner(), 7)), 0, "into_inner");
    assert_eq!(
        points(|| assert!(!format!("{:?}", Mutex::new(7)).is_empty())),
        0,
        "Debug"
    );
}

#[test]
fn mutex_flavour_points() {
    assert_eq!(points(|| drop(SpinMutex::<_>::new(0).lock())), 2, "SpinMutex");
    assert_eq!(
        points(|| drop(SpinMutex::<_>::new(0).try_lock_weak())),
        2,
        "try_lock_weak"
    );
    assert_eq!(points(|| drop(TicketMutex::<_>::new(0).lock())), 2, "TicketMutex");
    assert_eq!(
        points(|| drop(TicketMutex::<_>::new(0).try_lock())),
        2,
        "ticket try_lock"
    );
    assert_eq!(points(|| drop(FairMutex::<_>::new(0).lock())), 2, "FairMutex");
    assert_eq!(
        points(|| {
            let mutex = FairMutex::<_>::new(0);
            let starving = mutex.starve();
            drop(starving.try_lock().ok().unwrap());
        }),
        3,
        "starve, try_lock (which also ends the starving), release"
    );
    assert_eq!(
        points(|| {
            let mutex = FairMutex::<_>::new(0);
            drop(mutex.starve());
        }),
        2,
        "starve, its release"
    );
}

#[test]
fn rwlock_points() {
    assert_eq!(points(|| drop(RwLock::new(0).read())), 2, "read, release");
    assert_eq!(points(|| drop(RwLock::new(0).write())), 2, "write, release");
    assert_eq!(
        points(|| drop(RwLock::new(0).upgradeable_read())),
        2,
        "upgradeable, release"
    );
    assert_eq!(points(|| drop(RwLock::new(0).try_read())), 2, "try_read, release");
    assert_eq!(
        points(|| {
            let lock = RwLock::new(0);
            let _read = lock.read();
            assert!(lock.try_write().is_none());
        }),
        3,
        "read, failed try_write, release"
    );
    assert_eq!(
        points(|| drop(RwLock::new(0).upgradeable_read().upgrade())),
        3,
        "upgradeable, upgrade, release"
    );
    assert_eq!(
        points(|| drop(RwLock::new(0).upgradeable_read().try_upgrade().ok().unwrap())),
        3,
        "upgradeable, try_upgrade, release"
    );
    assert_eq!(
        points(|| drop(RwLock::new(0).write().downgrade())),
        3,
        "write, downgrade, release"
    );
    assert_eq!(
        points(|| drop(RwLock::new(0).write().downgrade_to_upgradeable())),
        3,
        "write, downgrade_to_upgradeable, release"
    );
    assert_eq!(
        points(|| drop(RwLock::new(0).upgradeable_read().downgrade())),
        3,
        "upgradeable, downgrade, release"
    );
    assert_eq!(
        points(|| assert_eq!(RwLock::new(0).reader_count(), 0)),
        1,
        "reader_count"
    );
    assert_eq!(
        points(|| assert_eq!(RwLock::new(0).writer_count(), 0)),
        1,
        "writer_count"
    );
    assert_eq!(points(|| assert_eq!(*RwLock::new(7).get_mut(), 7)), 0, "get_mut");
    assert_eq!(
        points(|| assert!(!format!("{:?}", RwLock::new(7)).is_empty())),
        0,
        "Debug"
    );
}

#[test]
fn once_points() {
    assert_eq!(
        points(|| {
            let once = Once::<u32>::new();
            assert_eq!(*once.call_once(|| 1), 1);
        }),
        2,
        "claim the initialization, publish the value"
    );
    assert_eq!(
        points(|| {
            let once = Once::<u32>::new();
            once.call_once(|| 1);
            // A complete `Once` is read without taking a scheduling point, as Shuttle's
            // `OnceLock` is.
            assert_eq!(*once.call_once(|| 2), 1);
            assert_eq!(once.get(), Some(&1));
            assert_eq!(once.poll(), Some(&1));
            assert_eq!(*once.wait(), 1);
            assert!(once.is_completed());
        }),
        2,
        "the second call_once, get, poll, wait and is_completed take none"
    );
    assert_eq!(
        points(|| {
            let once = Once::<u32>::new();
            assert!(once.get().is_none());
            assert!(once.poll().is_none());
            assert!(!once.is_completed());
        }),
        0,
        "an empty Once is read without a scheduling point"
    );
    assert_eq!(
        points(|| {
            let once = Once::<u32>::new();
            assert!(once.try_call_once(|| Err::<u32, ()>(())).is_err());
        }),
        2,
        "claim the initialization, give it up again"
    );
    assert_eq!(
        points(|| {
            let lazy: Lazy<u32> = Lazy::new(|| 1);
            assert_eq!(*lazy, 1);
            assert_eq!(*lazy, 1);
        }),
        2,
        "Lazy is a Once"
    );
    assert_eq!(
        points(|| assert_eq!(*Once::initialized(1u32).call_once(|| 2), 1)),
        0,
        "a Once that was made initialized needs no synchronization"
    );
}

#[test]
fn barrier_points() {
    assert_eq!(
        points(|| assert!(Barrier::new(1).wait().is_leader())),
        1,
        "the leader's arrival"
    );
    assert_eq!(
        points(|| {
            let barrier = Barrier::new(1);
            barrier.wait();
            barrier.wait();
        }),
        2,
        "one per arrival"
    );
}

#[test]
fn relax_points() {
    use spin::relax::{Loop, RelaxStrategy, Spin, Yield};

    assert_eq!(points(Spin::relax), 1, "spin_loop");
    assert_eq!(points(Loop::relax), 1, "spin_loop");
    assert_eq!(points(Yield::relax), 1, "yield_now");
}

/// Every arrival order of two tasks at a mutex is reachable, which is what the scheduling point
/// before taking the lock is for.
#[test]
fn both_lock_orders_are_reachable() {
    use shuttle::thread;
    use std::sync::Arc;

    let orders = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    let seen = orders.clone();
    check_random(
        move || {
            let mutex = Arc::new(Mutex::new(Vec::new()));
            let threads: Vec<_> = (0..2)
                .map(|i| {
                    let mutex = mutex.clone();
                    thread::spawn(move || mutex.lock().push(i))
                })
                .collect();
            for thread in threads {
                thread.join().unwrap();
            }
            seen.lock().unwrap().insert(mutex.lock().clone());
        },
        50,
    );
    let orders = orders.lock().unwrap();
    assert_eq!(orders.len(), 2, "both orders: {orders:?}");
}
