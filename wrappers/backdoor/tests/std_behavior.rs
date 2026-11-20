//! The backdoor items are the `std` items themselves, and they behave as `std` documents on
//! real OS threads.

// These tests name the `std` items to compare the backdoor's with them, and the crate's
// `clippy.toml` bans those paths.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]

use std::any::TypeId;
use std::sync::Arc;
use std::time::{Duration, Instant};

use shuttle_backdoor::collections;
use shuttle_backdoor::hint;
use shuttle_backdoor::sync::atomic::{
    AtomicBool, AtomicI8, AtomicI16, AtomicI32, AtomicI64, AtomicIsize, AtomicPtr, AtomicU8, AtomicU16, AtomicU32,
    AtomicU64, AtomicUsize, Ordering, fence,
};
use shuttle_backdoor::sync::mpsc::{self, RecvTimeoutError, TryRecvError, TrySendError};
use shuttle_backdoor::sync::{
    Barrier, Condvar, LazyLock, Mutex, MutexGuard, Once, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard,
};
use shuttle_backdoor::thread;

/// Fails, naming both types, unless they are the same type.
macro_rules! assert_same_type {
    ($backdoor:ty, $std:ty) => {
        assert_eq!(
            TypeId::of::<$backdoor>(),
            TypeId::of::<$std>(),
            "{} is not {}",
            stringify!($backdoor),
            stringify!($std),
        );
    };
}

#[test]
fn every_type_is_the_std_type() {
    assert_same_type!(Mutex<u8>, std::sync::Mutex<u8>);
    assert_same_type!(MutexGuard<'static, u8>, std::sync::MutexGuard<'static, u8>);
    assert_same_type!(RwLock<u8>, std::sync::RwLock<u8>);
    assert_same_type!(RwLockReadGuard<'static, u8>, std::sync::RwLockReadGuard<'static, u8>);
    assert_same_type!(RwLockWriteGuard<'static, u8>, std::sync::RwLockWriteGuard<'static, u8>);
    assert_same_type!(Condvar, std::sync::Condvar);
    assert_same_type!(Barrier, std::sync::Barrier);
    assert_same_type!(Once, std::sync::Once);
    assert_same_type!(OnceLock<u8>, std::sync::OnceLock<u8>);
    assert_same_type!(LazyLock<u8>, std::sync::LazyLock<u8>);

    assert_same_type!(AtomicBool, std::sync::atomic::AtomicBool);
    assert_same_type!(AtomicU8, std::sync::atomic::AtomicU8);
    assert_same_type!(AtomicU16, std::sync::atomic::AtomicU16);
    assert_same_type!(AtomicU32, std::sync::atomic::AtomicU32);
    assert_same_type!(AtomicU64, std::sync::atomic::AtomicU64);
    assert_same_type!(AtomicUsize, std::sync::atomic::AtomicUsize);
    assert_same_type!(AtomicI8, std::sync::atomic::AtomicI8);
    assert_same_type!(AtomicI16, std::sync::atomic::AtomicI16);
    assert_same_type!(AtomicI32, std::sync::atomic::AtomicI32);
    assert_same_type!(AtomicI64, std::sync::atomic::AtomicI64);
    assert_same_type!(AtomicIsize, std::sync::atomic::AtomicIsize);
    assert_same_type!(AtomicPtr<u8>, std::sync::atomic::AtomicPtr<u8>);
    assert_same_type!(Ordering, std::sync::atomic::Ordering);

    assert_same_type!(mpsc::Sender<u8>, std::sync::mpsc::Sender<u8>);
    assert_same_type!(mpsc::SyncSender<u8>, std::sync::mpsc::SyncSender<u8>);
    assert_same_type!(mpsc::Receiver<u8>, std::sync::mpsc::Receiver<u8>);

    assert_same_type!(thread::JoinHandle<u8>, std::thread::JoinHandle<u8>);
    assert_same_type!(
        thread::ScopedJoinHandle<'static, u8>,
        std::thread::ScopedJoinHandle<'static, u8>
    );
    assert_same_type!(thread::Thread, std::thread::Thread);
    assert_same_type!(thread::ThreadId, std::thread::ThreadId);

    assert_same_type!(collections::RandomState, std::hash::RandomState);
    assert_same_type!(
        collections::HashMap<u8, u8>,
        std::collections::HashMap<u8, u8>
    );
    assert_same_type!(collections::HashSet<u8>, std::collections::HashSet<u8>);
}

#[test]
fn locks_hold_unsized_values() {
    let call: Arc<Mutex<dyn Fn() -> u32 + Send>> = Arc::new(Mutex::new(|| 7));
    assert_eq!((call.lock().unwrap())(), 7);

    let bytes: Box<RwLock<[u8]>> = Box::new(RwLock::new([1, 2, 3]));
    assert_eq!(bytes.read().unwrap().len(), 3);
}

#[test]
fn mutex_counts_across_real_threads() {
    let counter = Arc::new(Mutex::new(0));
    let workers: Vec<thread::JoinHandle<()>> = (0..4)
        .map(|_| {
            let counter = Arc::clone(&counter);
            thread::spawn(move || {
                for _ in 0..1000 {
                    *counter.lock().unwrap() += 1;
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(*counter.lock().unwrap(), 4000);
}

#[test]
fn rwlock_admits_two_readers_or_one_writer() {
    let lock = RwLock::new(5);
    let first = lock.read().unwrap();
    let second = lock.read().unwrap();
    assert_eq!(*first + *second, 10);
    assert!(lock.try_write().is_err());
    drop((first, second));

    *lock.write().unwrap() += 1;
    assert_eq!(*lock.read().unwrap(), 6);
}

#[test]
fn condvar_wakes_a_waiting_thread() {
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let notifier = {
        let pair = Arc::clone(&pair);
        thread::spawn(move || {
            let (ready, condvar) = &*pair;
            *ready.lock().unwrap() = true;
            condvar.notify_one();
        })
    };

    let (ready, condvar) = &*pair;
    let ready = condvar.wait_while(ready.lock().unwrap(), |ready| !*ready).unwrap();
    assert!(*ready);
    drop(ready);
    notifier.join().unwrap();
}

#[test]
fn barrier_releases_every_thread_and_elects_one_leader() {
    const THREADS: usize = 3;
    let barrier = Arc::new(Barrier::new(THREADS));
    let waiters: Vec<_> = (0..THREADS)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || barrier.wait().is_leader())
        })
        .collect();
    let leaders = waiters
        .into_iter()
        .map(|waiter| waiter.join().unwrap())
        .filter(|&is_leader| is_leader)
        .count();
    assert_eq!(leaders, 1);
}

#[test]
fn once_cells_initialize_exactly_once() {
    static ONCE: Once = Once::new();
    static ONCE_RUNS: AtomicUsize = AtomicUsize::new(0);
    static CELL: OnceLock<usize> = OnceLock::new();
    static LAZY_RUNS: AtomicUsize = AtomicUsize::new(0);
    static LAZY: LazyLock<usize> = LazyLock::new(|| {
        LAZY_RUNS.fetch_add(1, Ordering::SeqCst);
        42
    });

    let readers: Vec<_> = (0..4)
        .map(|i| {
            thread::spawn(move || {
                ONCE.call_once(|| {
                    ONCE_RUNS.fetch_add(1, Ordering::SeqCst);
                });
                (*CELL.get_or_init(|| i), *LAZY)
            })
        })
        .collect();
    let seen: Vec<(usize, usize)> = readers.into_iter().map(|reader| reader.join().unwrap()).collect();

    assert!(ONCE.is_completed());
    assert_eq!(ONCE_RUNS.load(Ordering::SeqCst), 1);
    assert_eq!(LAZY_RUNS.load(Ordering::SeqCst), 1);
    let cell = *CELL.get().unwrap();
    assert!(seen.iter().all(|&pair| pair == (cell, 42)));
}

#[test]
fn atomics_update_in_place() {
    macro_rules! check_integer_atomics {
        ($($atomic:ty),*) => {$(
            let value = <$atomic>::new(1);
            assert_eq!(value.fetch_add(2, Ordering::SeqCst), 1);
            assert_eq!(value.load(Ordering::Acquire), 3);
        )*};
    }
    check_integer_atomics!(
        AtomicU8,
        AtomicU16,
        AtomicU32,
        AtomicU64,
        AtomicUsize,
        AtomicI8,
        AtomicI16,
        AtomicI32,
        AtomicI64,
        AtomicIsize
    );

    let flag = AtomicBool::new(false);
    assert!(!flag.swap(true, Ordering::AcqRel));
    assert!(flag.load(Ordering::Relaxed));

    let mut target = 5_u8;
    let pointer = AtomicPtr::new(std::ptr::null_mut());
    pointer.store(&mut target, Ordering::Release);
    assert_eq!(pointer.load(Ordering::Acquire), &mut target as *mut u8);
}

#[test]
fn channel_carries_messages_between_threads() {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for i in 0..3 {
            sender.send(i).unwrap();
        }
    })
    .join()
    .unwrap();
    assert_eq!(receiver.iter().collect::<Vec<_>>(), vec![0, 1, 2]);
    assert_eq!(receiver.try_recv(), Err(TryRecvError::Disconnected));
}

#[test]
fn sync_channel_applies_its_bound() {
    let (sender, receiver) = mpsc::sync_channel(1);
    sender.try_send(1).unwrap();
    assert_eq!(sender.try_send(2), Err(TrySendError::Full(2)));
    assert_eq!(receiver.recv_timeout(Duration::from_millis(10)), Ok(1));
    assert_eq!(
        receiver.recv_timeout(Duration::from_millis(10)),
        Err(RecvTimeoutError::Timeout)
    );
}

#[test]
fn spawn_runs_on_another_os_thread() {
    let here = std::thread::current().id();
    let there = thread::spawn(|| std::thread::current().id()).join().unwrap();
    assert_ne!(here, there);
}

#[test]
fn builder_spawns_a_named_thread() {
    let named = thread::Builder::new()
        .name("backdoor-builder".to_string())
        .stack_size(256 * 1024)
        .spawn(|| std::thread::current().name().map(str::to_owned))
        .unwrap();
    assert_eq!(named.join().unwrap().as_deref(), Some("backdoor-builder"));

    let unnamed = thread::Builder::default().spawn(|| 7).unwrap();
    assert_eq!(unnamed.join().unwrap(), 7);
}

#[test]
fn sleep_waits_real_time() {
    let start = Instant::now();
    thread::sleep(Duration::from_millis(20));
    assert!(start.elapsed() >= Duration::from_millis(20));
}

#[test]
fn scope_borrows_from_the_caller_and_joins_every_thread() {
    let greeting = String::from("scoped");
    let mut spawned = 0_usize;

    let ids = thread::scope(|scope| {
        let first = scope.spawn(|| (greeting.len(), thread::current().id()));
        // `Scope` is `Copy`, so the same scope goes to a builder as well.
        let second = thread::Builder::new()
            .name("backdoor-scoped".to_string())
            .spawn_scoped(scope, || (greeting.len(), thread::current().name().map(str::to_owned)))
            .unwrap();
        spawned += 2;
        (first.join().unwrap(), second.join().unwrap())
    });

    assert_eq!(spawned, 2);
    let ((first_len, first_id), (second_len, second_name)) = ids;
    assert_eq!(first_len, greeting.len());
    assert_eq!(second_len, greeting.len());
    assert_ne!(first_id, thread::current().id());
    assert_eq!(second_name.as_deref(), Some("backdoor-scoped"));

    // A scope joins its threads before it returns, even when nothing joins them by hand.
    let ran = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| ran.store(true, Ordering::SeqCst));
    });
    assert!(ran.load(Ordering::SeqCst));
}

#[test]
fn yield_now_returns_to_the_caller() {
    let done = Arc::new(AtomicBool::new(false));
    let worker = {
        let done = Arc::clone(&done);
        thread::spawn(move || done.store(true, Ordering::SeqCst))
    };
    while !done.load(Ordering::SeqCst) {
        thread::yield_now();
    }
    worker.join().unwrap();
}

#[test]
fn park_returns_after_an_unpark() {
    // `unpark` leaves a token behind, so this `park` returns at once.
    thread::current().unpark();
    thread::park();

    // And an `unpark` from another OS thread releases this thread's `park`, whether it comes
    // before the `park` starts or while it waits.
    let here: thread::Thread = thread::current();
    let waker = thread::spawn(move || here.unpark());
    thread::park();
    waker.join().unwrap();
}

#[test]
fn park_timeout_waits_real_time() {
    let start = Instant::now();
    thread::park_timeout(Duration::from_millis(20));
    assert!(start.elapsed() >= Duration::from_millis(20));
}

#[test]
fn current_is_the_real_os_thread() {
    let here = thread::current();
    assert_eq!(here.id(), std::thread::current().id());

    let there = thread::spawn(|| thread::current().id()).join().unwrap();
    assert_ne!(here.id(), there);
}

#[test]
fn available_parallelism_reports_the_host() {
    assert_eq!(
        thread::available_parallelism().ok(),
        std::thread::available_parallelism().ok()
    );
    assert!(thread::available_parallelism().unwrap().get() >= 1);
}

#[test]
fn fence_and_spin_loop_complete_a_handoff_between_real_threads() {
    // This shows that both calls run and the handoff completes. It can't show the ordering
    // itself, which a strongly ordered host such as x86 gives even without the fences.
    static DATA: AtomicU32 = AtomicU32::new(0);
    static READY: AtomicBool = AtomicBool::new(false);

    let writer = thread::spawn(|| {
        DATA.store(7, Ordering::Relaxed);
        fence(Ordering::Release);
        READY.store(true, Ordering::Relaxed);
    });
    while !READY.load(Ordering::Relaxed) {
        hint::spin_loop();
    }
    fence(Ordering::Acquire);
    assert_eq!(DATA.load(Ordering::Relaxed), 7);
    writer.join().unwrap();
}

#[test]
fn hash_collections_are_accepted_where_std_ones_are() {
    fn count_std(map: &std::collections::HashMap<u32, u32>) -> usize {
        map.len()
    }

    let mut map: collections::HashMap<u32, u32> = collections::HashMap::new();
    map.insert(1, 2);
    assert_eq!(count_std(&map), 1);

    let set: collections::HashSet<u32> = collections::HashSet::from_iter([1, 2, 3]);
    assert_eq!(set.len(), 3);

    // The hasher state is `std`'s own, so it goes where `std::hash::RandomState` is expected.
    let state: collections::RandomState = collections::RandomState::new();
    let empty: std::collections::HashMap<u32, u32, std::hash::RandomState> =
        std::collections::HashMap::with_hasher(state);
    assert!(empty.is_empty());
}
