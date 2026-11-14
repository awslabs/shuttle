//! Code written against spin, as a crate that depends on `shuttle-spin` under the name `spin` would
//! have it. The same source is compiled against `shuttle-spin` without the `shuttle` feature, where
//! it is spin, and against `shuttle-spin-inner`, which is what `shuttle-spin` is under `shuttle`, so
//! every check here holds for spin and for the model alike. It runs on one task, so it checks
//! spin's single-threaded rules (what each `try_*` returns, the counts, the formats, leaking); the
//! inner crate's tests check the behaviour across tasks.
//!
//! The test that includes it names the crate `spin`, with the `fair_mutex`, `ticket_mutex` and
//! `std` features on.

use spin::mutex::{FairMutex, SpinMutex, SpinMutexGuard, TicketMutex, TicketMutexGuard};
use spin::relax::{Loop, RelaxStrategy, Spin};
use spin::{Barrier, Lazy, Mutex, MutexGuard, Once, RwLock, RwLockReadGuard, RwLockUpgradableGuard, RwLockWriteGuard};

// Statics, as a consumer declares them. Under Shuttle, two executions on different threads can't
// hold one of these locks at once (see the inner crate's docs): the Shuttle tests that run this code
// take turns.
static STATIC_MUTEX: Mutex<u32> = Mutex::new(1);
static STATIC_RWLOCK: RwLock<u32> = RwLock::new(2);
static STATIC_ONCE: Once<u32> = Once::new();
static STATIC_LAZY: Lazy<u32> = Lazy::new(|| 4);
static STATIC_BARRIER: Barrier = Barrier::new(1);
static STATIC_TICKET: TicketMutex<u32> = TicketMutex::new(5);
static STATIC_FAIR: FairMutex<u32> = FairMutex::new(6);
static STATIC_INIT_ONCE: Once<&str> = Once::INIT;

/// Exercises spin's API.
pub fn exercise() {
    mutex();
    spin_mutex();
    ticket_mutex();
    fair_mutex();
    rwlock();
    upgradeable();
    once();
    lazy();
    barrier();
    lock_api();
    relax();
    statics();
}

fn mutex() {
    let mutex = Mutex::new(0u32);
    {
        let mut guard: MutexGuard<'_, u32> = mutex.lock();
        *guard += 1;
        assert!(mutex.is_locked());
        assert!(mutex.try_lock().is_none());
        assert_eq!(format!("{guard:?}"), "1");
        assert_eq!(guard.to_string(), "1");
        assert_eq!(format!("{mutex:?}"), "Mutex { <locked> }");
    }
    assert!(!mutex.is_locked());
    assert_eq!(format!("{mutex:?}"), "Mutex { data: 1 }");
    *mutex.try_lock().unwrap() += 1;

    let data: &mut u32 = MutexGuard::leak(mutex.lock());
    *data += 1;
    assert!(mutex.is_locked());
    assert!(mutex.try_lock().is_none());
    // Safety: the guard was leaked, and `data` is not used again.
    unsafe { mutex.force_unlock() };
    assert_eq!(*mutex.lock(), 3);

    let mut mutex = mutex;
    *mutex.get_mut() += 1;
    assert_eq!(mutex.into_inner(), 4);
    assert_eq!(Mutex::<u32>::default().into_inner(), 0);
    assert_eq!(Mutex::from(5u32).into_inner(), 5);

    // Unsized data, by coercion.
    let boxed: Box<Mutex<[u8]>> = Box::new(Mutex::new([1, 2, 3]));
    boxed.lock()[1] = 5;
    assert_eq!(&*boxed.lock(), &[1, 5, 3]);
}

fn spin_mutex() {
    let mutex = SpinMutex::<_>::new(vec![1]);
    {
        let mut guard: SpinMutexGuard<'_, Vec<i32>> = mutex.lock();
        guard.push(2);
        assert!(mutex.try_lock_weak().is_none());
        assert_eq!(format!("{mutex:?}"), "Mutex { <locked> }");
    }
    assert_eq!(*mutex.try_lock_weak().unwrap(), [1, 2]);
    assert_eq!(format!("{mutex:?}"), "Mutex { data: [1, 2] }");
    // Safety: nothing else holds the lock.
    unsafe { (*mutex.as_mut_ptr()).push(3) };
    let leaked = SpinMutexGuard::leak(mutex.lock());
    leaked.push(4);
    assert!(mutex.is_locked());
    // Safety: the guard was leaked, and `leaked` is not used again.
    unsafe { mutex.force_unlock() };
    assert_eq!(mutex.into_inner(), [1, 2, 3, 4]);

    let relaxed = SpinMutex::<u32, Loop>::new(1);
    *relaxed.lock() += 1;
    assert_eq!(*relaxed.lock(), 2);
}

fn ticket_mutex() {
    let mutex = TicketMutex::<_>::new(1u32);
    {
        let mut guard: TicketMutexGuard<'_, u32> = mutex.lock();
        *guard += 1;
        assert!(mutex.is_locked());
        assert!(mutex.try_lock().is_none());
        assert_eq!(format!("{mutex:?}"), "Mutex { <locked> }");
    }
    assert!(!mutex.is_locked());
    assert_eq!(format!("{mutex:?}"), "Mutex { data: 2 }");
    let data = TicketMutexGuard::leak(mutex.try_lock().unwrap());
    *data += 1;
    // Safety: the guard was leaked, and `data` is not used again.
    unsafe { mutex.force_unlock() };
    assert!(!mutex.is_locked());
    assert_eq!(*mutex.lock(), 3);
    assert_eq!(TicketMutex::<u32>::from(7).into_inner(), 7);
}

fn fair_mutex() {
    use spin::mutex::fair::LockRejectReason;

    let mutex = FairMutex::<_>::new(1u32);
    assert_eq!(format!("{mutex:?}"), "FairMutex { data: 1 }");
    let guard = mutex.lock();
    assert!(matches!(mutex.try_lock_starver(), Err(LockRejectReason::Locked)));
    assert_eq!(format!("{mutex:?}"), "FairMutex { data: <locked> }");
    // A user who has waited for a while marks themselves as starving: a newcomer is refused, even
    // once the lock is free, and the starving user gets it.
    let starving = mutex.starve();
    drop(guard);
    assert!(!mutex.is_locked());
    assert!(matches!(mutex.try_lock_starver(), Err(LockRejectReason::Starved)));
    assert!(mutex.try_lock().is_none());
    assert_eq!(format!("{mutex:?}"), "FairMutex { data: <locked> }");
    let mut guard = starving.try_lock_fair().ok().unwrap();
    *guard += 1;
    drop(guard);
    assert_eq!(*mutex.try_lock().unwrap(), 2);

    // Two starving users: neither is the only one, and `try_lock` ignores the other.
    let first = mutex.starve();
    let second = mutex.starve();
    let second = second.try_lock_fair().err().unwrap();
    let guard = second.try_lock().ok().unwrap();
    let first = first.try_lock().err().unwrap();
    drop(guard);
    drop(first.lock());
    assert_eq!(LockRejectReason::Locked.to_string(), "locked");
    assert_eq!(LockRejectReason::Starved.to_string(), "starved");
    let error: &dyn std::error::Error = &LockRejectReason::Starved;
    assert_eq!(error.to_string(), "starved");

    let data = spin::mutex::FairMutexGuard::leak(mutex.lock());
    *data += 1;
    assert!(mutex.is_locked());
    // Safety: the guard was leaked, and `data` is not used again.
    unsafe { mutex.force_unlock() };
    assert_eq!(mutex.into_inner(), 3);
}

fn rwlock() {
    let lock = RwLock::new(5u32);
    assert_eq!(format!("{lock:?}"), "RwLock { data: 5 }");
    {
        let first: RwLockReadGuard<'_, u32> = lock.read();
        let second = lock.try_read().unwrap();
        assert_eq!(*first + *second, 10);
        assert_eq!(lock.reader_count(), 2);
        assert_eq!(lock.writer_count(), 0);
        assert!(lock.try_write().is_none());
        assert_eq!(format!("{lock:?}"), "RwLock { data: 5 }");
        assert_eq!(first.to_string(), "5");
    }
    {
        let mut writer: RwLockWriteGuard<'_, u32> = lock.write();
        *writer += 1;
        assert_eq!(lock.writer_count(), 1);
        assert!(lock.try_read().is_none());
        assert!(lock.try_write_weak().is_none());
        assert_eq!(format!("{lock:?}"), "RwLock { <locked> }");
        // A failed upgradeable attempt leaves its bit set until the writer releases the lock,
        // and the bit counts as a reader.
        assert!(lock.try_upgradeable_read().is_none());
        assert_eq!(lock.reader_count(), 1);
        let reader = RwLockWriteGuard::downgrade(writer);
        assert_eq!(*reader, 6);
        assert_eq!(lock.reader_count(), 1);
        assert_eq!(lock.writer_count(), 0);
    }
    assert_eq!(lock.reader_count(), 0);

    let leaked = RwLockReadGuard::leak(lock.read());
    assert_eq!(*leaked, 6);
    assert!(lock.try_write().is_none());
    // Safety: the guard was leaked, and `leaked` is not used again.
    unsafe { lock.force_read_decrement() };
    let leaked = RwLockWriteGuard::leak(lock.write());
    *leaked += 1;
    assert!(lock.try_read().is_none());
    // Safety: as above.
    unsafe { lock.force_write_unlock() };
    assert_eq!(*lock.try_read().unwrap(), 7);

    let mut lock = lock;
    *lock.get_mut() += 1;
    // Safety: nothing holds the lock.
    unsafe { *lock.as_mut_ptr() += 1 };
    assert_eq!(lock.into_inner(), 9);
    assert_eq!(RwLock::<u32>::default().into_inner(), 0);
    assert_eq!(RwLock::from(3u32).into_inner(), 3);

    let boxed: Box<RwLock<[u8]>> = Box::new(RwLock::new([1, 2]));
    boxed.write()[0] = 3;
    assert_eq!(&*boxed.read(), &[3, 2]);
}

fn upgradeable() {
    let lock = RwLock::new(1u32);
    let reader = lock.read();
    let upgradeable: RwLockUpgradableGuard<'_, u32> = lock.upgradeable_read();
    // An upgradeable guard keeps new readers, writers and other upgradeable guards out, and lets
    // the readers that are in the lock stay.
    assert!(lock.try_read().is_none());
    assert!(lock.try_write().is_none());
    assert!(lock.try_upgradeable_read().is_none());
    assert_eq!(lock.reader_count(), 2);
    assert_eq!(format!("{lock:?}"), "RwLock { <locked> }");
    assert_eq!(format!("{upgradeable:?}"), "1");
    let upgradeable = upgradeable.try_upgrade().err().unwrap();
    let upgradeable = upgradeable.try_upgrade_weak().err().unwrap();
    drop(reader);
    let mut writer = upgradeable.upgrade();
    *writer += 1;
    let upgradeable = writer.downgrade_to_upgradeable();
    assert_eq!(*upgradeable, 2);
    assert!(lock.try_read().is_none());
    let reader = upgradeable.downgrade();
    assert!(lock.try_read().is_some());
    assert!(lock.try_upgradeable_read().is_some());
    drop(reader);
    let writer = lock.try_upgradeable_read().unwrap().try_upgrade().ok().unwrap();
    assert_eq!(*writer, 2);
    drop(writer);

    let leaked = RwLockUpgradableGuard::leak(lock.upgradeable_read());
    assert_eq!(*leaked, 2);
    assert!(lock.try_read().is_none());
    // Safety: the guard was leaked, and `leaked` is not used again.
    unsafe { lock.force_write_unlock() };
    assert!(lock.try_write().is_some());
}

fn once() {
    let once = Once::<u32>::new();
    assert!(once.get().is_none());
    assert!(once.poll().is_none());
    assert!(!once.is_completed());
    assert_eq!(format!("{once:?}"), "Once(<uninit>)");
    assert_eq!(once.try_call_once(|| Err::<u32, &str>("no")), Err("no"));
    assert!(!once.is_completed());
    assert_eq!(*once.call_once(|| 1), 1);
    assert_eq!(*once.call_once(|| 2), 1);
    assert_eq!(once.try_call_once(|| Err::<u32, &str>("no")), Ok(&1));
    assert_eq!(once.get(), Some(&1));
    assert_eq!(once.poll(), Some(&1));
    assert_eq!(*once.wait(), 1);
    assert!(once.is_completed());
    assert_eq!(format!("{once:?}"), "Once(1)");
    // Safety: the `Once` is initialized.
    assert_eq!(unsafe { *once.get_unchecked() }, 1);
    // Safety: as above.
    assert_eq!(unsafe { *once.as_mut_ptr() }, 1);

    let mut once = once;
    *once.get_mut().unwrap() += 1;
    // Safety: as above.
    unsafe { *once.get_mut_unchecked() += 1 };
    assert_eq!(once.try_into_inner(), Some(3));
    assert_eq!(Once::<u32>::new().try_into_inner(), None);
    assert!(Once::<u32>::default().get().is_none());

    let initialized = Once::<_>::initialized(vec![1]);
    assert_eq!(initialized.get(), Some(&vec![1]));
    assert_eq!(*initialized.call_once(|| vec![2]), [1]);
    assert_eq!(Once::<u32>::from(4).get(), Some(&4));
    // Safety: initialized.
    assert_eq!(unsafe { initialized.into_inner_unchecked() }, [1]);

    // A panicking initializer poisons the `Once`.
    let poisoned = Once::<u32>::new();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        poisoned.call_once(|| panic!("initializer"));
    }));
    assert!(caught.is_err());
    assert!(poisoned.get().is_none());
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        poisoned.call_once(|| 1);
    }))
    .unwrap_err();
    assert_eq!(panic.downcast_ref::<&str>(), Some(&"Once panicked"));
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        poisoned.poll();
    }))
    .unwrap_err();
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"Once previously poisoned by a panicked")
    );

    // `std::sync::Once`'s use.
    let start = Once::<()>::new();
    start.call_once(|| ());
    assert!(start.is_completed());
}

fn lazy() {
    let lazy: Lazy<Vec<u32>> = Lazy::new(|| vec![1, 2]);
    assert_eq!(format!("{lazy:?}"), "Lazy(<uninit>)");
    assert_eq!(lazy.len(), 2);
    assert_eq!(Lazy::force(&lazy), &[1, 2]);
    assert_eq!(format!("{lazy:?}"), "Lazy([1, 2])");
    // Safety: initialized, and nothing else uses it.
    unsafe { (*lazy.as_mut_ptr()).push(3) };
    assert_eq!(*lazy, [1, 2, 3]);
    let default: Lazy<u32> = Lazy::default();
    assert_eq!(*default, 0);
    let relaxed = spin::lazy::Lazy::<u32, fn() -> u32, Loop>::new(|| 7);
    assert_eq!(*relaxed, 7);
}

fn barrier() {
    let barrier = Barrier::new(1);
    assert!(barrier.wait().is_leader());
    assert!(barrier.wait().is_leader());
    assert!(spin::barrier::Barrier::<Spin>::new(0).wait().is_leader());
}

fn lock_api() {
    let mutex = spin::lock_api::Mutex::new(1u32);
    {
        let mut guard: spin::lock_api::MutexGuard<'_, u32> = mutex.lock();
        *guard += 1;
        assert!(mutex.is_locked());
        assert!(mutex.try_lock().is_none());
    }
    assert_eq!(*mutex.lock(), 2);

    let lock = spin::lock_api::RwLock::new(1u32);
    {
        let reader: spin::lock_api::RwLockReadGuard<'_, u32> = lock.read();
        assert!(lock.try_write().is_none());
        assert!(lock.is_locked());
        drop(reader);
        let upgradable: spin::lock_api::RwLockUpgradableReadGuard<'_, u32> = lock.upgradable_read();
        assert!(lock.try_read().is_none());
        let mut writer: spin::lock_api::RwLockWriteGuard<'_, u32> =
            spin::lock_api::RwLockUpgradableReadGuard::upgrade(upgradable);
        *writer += 1;
        let reader = spin::lock_api::RwLockWriteGuard::downgrade(writer);
        assert_eq!(*reader, 2);
        assert!(lock.try_read().is_some());
    }
    let upgradable = lock.upgradable_read();
    let upgradable = spin::lock_api::RwLockUpgradableReadGuard::try_upgrade(upgradable).err();
    assert!(upgradable.is_none());
    assert!(!lock.is_locked());
}

fn relax() {
    fn relax_with<R: RelaxStrategy>() {
        R::relax();
    }
    relax_with::<Spin>();
    relax_with::<Loop>();
    relax_with::<spin::Yield>();
}

fn statics() {
    {
        let mut guard = STATIC_MUTEX.lock();
        assert_eq!(*guard, 1, "a static's value starts afresh");
        *guard += 1;
    }
    {
        let mut guard = STATIC_RWLOCK.write();
        assert_eq!(*guard, 2);
        *guard += 1;
    }
    assert_eq!(*STATIC_ONCE.call_once(|| 3), 3);
    assert_eq!(*STATIC_LAZY, 4);
    assert!(STATIC_BARRIER.wait().is_leader());
    {
        let mut guard = STATIC_TICKET.lock();
        assert_eq!(*guard, 5);
        *guard += 1;
    }
    {
        let mut guard = STATIC_FAIR.lock();
        assert_eq!(*guard, 6);
        *guard += 1;
    }
    assert_eq!(*STATIC_INIT_ONCE.call_once(|| "init"), "init");
}
