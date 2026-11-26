//! Code written against `parking_lot`, as a crate that depends on `shuttle-parking_lot` under the
//! name `parking_lot` would have it. The module that includes this one provides `parking_lot` and
//! `thread` beside it: `shuttle-parking_lot` and `std::thread` without the `shuttle` feature, the
//! Shuttle implementation and `shuttle::thread` with it, and real `parking_lot` and `std::thread`
//! in `shuttle-parking_lot-impl`'s parity test. So the same source compiles against all three, which
//! is what the signatures below pin.
//!
//! The checks hold whatever runs the code: OS threads, or Shuttle tasks under any schedule.

#![allow(dead_code)]

use super::parking_lot::{
    self, Condvar, FairMutex, FairMutexGuard, MappedFairMutexGuard, MappedMutexGuard, MappedReentrantMutexGuard,
    MappedRwLockReadGuard, MappedRwLockWriteGuard, Mutex, MutexGuard, Once, OnceState, RawFairMutex, RawMutex,
    RawRwLock, RawThreadId, ReentrantMutex, ReentrantMutexGuard, RwLock, RwLockReadGuard, RwLockUpgradableReadGuard,
    RwLockWriteGuard, WaitTimeoutResult,
};
use super::thread;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Statics, as a consumer declares them. None holds data. Under Shuttle, a lock's state is per
/// execution but its data is not, so two executions on different threads can't hold one at once
/// (see the crate docs): the Shuttle tests that run this code take turns.
static INIT: Once = Once::new();
static WAKE: Condvar = Condvar::new();
static GATE: Mutex<()> = parking_lot::const_mutex(());
static GATE_RW: RwLock<()> = parking_lot::const_rwlock(());
static GATE_FAIR: FairMutex<()> = parking_lot::const_fair_mutex(());
static GATE_NEW: Mutex<()> = Mutex::new(());

/// Exercises the API.
pub fn exercise() {
    mutexes();
    rwlocks();
    condvars();
    reentrant_mutexes();
    fair_mutexes();
    onces();
    statics();
}

fn mutexes() {
    let counter = Arc::new(Mutex::new(0u32));
    let workers = (0..2)
        .map(|_| {
            let counter = Arc::clone(&counter);
            thread::spawn(move || {
                *counter.lock() += 1;
                if let Some(mut guard) = counter.try_lock_for(Duration::from_secs(5)) {
                    *guard += 1;
                    MutexGuard::bump(&mut guard);
                    *guard += 1;
                    MutexGuard::unlock_fair(guard);
                }
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(*counter.lock(), 6);

    let pair = Mutex::new((1, 2));
    let mapped: MappedMutexGuard<'_, i32> = MutexGuard::map(pair.lock(), |pair| &mut pair.1);
    assert_eq!(*mapped, 2);
    drop(mapped);
    let unlocked = {
        let mut guard = pair.lock();
        MutexGuard::unlocked(&mut guard, || pair.try_lock().is_some())
    };
    assert!(unlocked);
    assert!(!pair.is_locked());
    let mut pair = pair;
    *pair.get_mut() = (3, 4);
    assert_eq!(pair.into_inner(), (3, 4));
    assert_eq!(*Mutex::<u8>::default().lock(), 0);
    assert_eq!(format!("{:?}", Mutex::new(7)), "Mutex { data: 7 }");
}

fn rwlocks() {
    let lock = Arc::new(RwLock::new(0u32));
    let reader = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || {
            let read = lock.read();
            let again = lock.read_recursive();
            assert_eq!(*read, *again);
        })
    };
    {
        let upgradable = lock.upgradable_read();
        let mut write = RwLockUpgradableReadGuard::upgrade(upgradable);
        *write += 1;
        let read = RwLockWriteGuard::downgrade(write);
        assert!(*read >= 1);
    }
    reader.join().unwrap();
    {
        let write = lock
            .try_write_for(Duration::from_secs(5))
            .expect("nobody holds the lock");
        let upgradable = RwLockWriteGuard::downgrade_to_upgradable(write);
        let read = RwLockUpgradableReadGuard::downgrade(upgradable);
        let mapped: MappedRwLockReadGuard<'_, u32> = RwLockReadGuard::map(read, |value| value);
        assert_eq!(*mapped, 1);
    }
    {
        let write = lock.write();
        let mut mapped: MappedRwLockWriteGuard<'_, u32> = RwLockWriteGuard::map(write, |value| value);
        *mapped += 1;
    }
    assert!(lock.try_read_until(Instant::now() + Duration::from_secs(5)).is_some());
    assert!(!lock.is_locked() && !lock.is_locked_exclusive());
    assert_eq!(*lock.read(), 2);
}

fn condvars() {
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let notifier = {
        let pair = Arc::clone(&pair);
        thread::spawn(move || {
            let (lock, condvar) = &*pair;
            *lock.lock() = true;
            condvar.notify_all();
        })
    };
    let (lock, condvar) = &*pair;
    let mut ready = lock.lock();
    let result: WaitTimeoutResult = condvar.wait_while_for(&mut ready, |ready| !*ready, Duration::from_secs(3600));
    // Nothing here takes an hour, whichever runs it.
    assert!(!result.timed_out());
    assert!(*ready);
    drop(ready);
    notifier.join().unwrap();
    assert!(!condvar.notify_one());
    assert_eq!(condvar.notify_all(), 0);
    assert_eq!(format!("{condvar:?}"), "Condvar { .. }");
    let _ = Condvar::default();
}

fn reentrant_mutexes() {
    let lock = Arc::new(ReentrantMutex::new(RefCell::new(Vec::new())));
    let other = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || {
            let outer = lock.lock();
            let inner = lock.lock();
            inner.borrow_mut().push(1);
            assert!(lock.is_owned_by_current_thread());
            drop((inner, outer));
            assert!(!lock.is_owned_by_current_thread());
        })
    };
    {
        let outer = lock.lock();
        let inner = lock.try_lock().expect("the owner locks it again");
        inner.borrow_mut().push(2);
        let mapped: MappedReentrantMutexGuard<'_, RefCell<Vec<i32>>> = ReentrantMutexGuard::map(inner, |cell| cell);
        mapped.borrow_mut().push(3);
        drop((mapped, outer));
    }
    other.join().unwrap();
    let mut values = lock.lock().borrow().clone();
    values.sort_unstable();
    assert_eq!(values, [1, 2, 3]);
    assert!(lock.try_lock_for(Duration::from_secs(5)).is_some());
}

fn fair_mutexes() {
    let lock = Arc::new(FairMutex::new(0u32));
    let other = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || *lock.lock() += 1)
    };
    {
        let mut guard: FairMutexGuard<'_, u32> = lock.lock();
        *guard += 1;
        FairMutexGuard::bump(&mut guard);
        let mapped: MappedFairMutexGuard<'_, u32> = FairMutexGuard::map(guard, |value| value);
        drop(mapped);
    }
    other.join().unwrap();
    assert_eq!(*lock.lock(), 2);
}

fn onces() {
    let once = Arc::new(Once::new());
    let runs = Arc::new(Mutex::new(0));
    let callers = (0..2)
        .map(|_| {
            let (once, runs) = (Arc::clone(&once), Arc::clone(&runs));
            thread::spawn(move || {
                once.call_once(|| *runs.lock() += 1);
                assert_eq!(once.state(), OnceState::Done);
            })
        })
        .collect::<Vec<_>>();
    for caller in callers {
        caller.join().unwrap();
    }
    assert_eq!(*runs.lock(), 1);
    let forced = Once::new();
    forced.call_once_force(|state| assert!(!state.poisoned() && !state.done()));
    assert!(forced.state().done());
    assert_eq!(format!("{forced:?}"), "Once { state: Done }");
}

fn statics() {
    INIT.call_once(|| {});
    assert!(INIT.state().done());
    drop(GATE.lock());
    drop(GATE_RW.write());
    drop(GATE_RW.read());
    drop(GATE_FAIR.lock());
    drop(GATE_NEW.lock());
    assert!(!WAKE.notify_one());
}

/// Every signature, as a fn pointer, so that a mismatch fails to compile.
#[allow(clippy::type_complexity)]
pub fn signatures() {
    type Pair = (u32, u32);

    // Mutex.
    let _: fn(u32) -> Mutex<u32> = Mutex::new;
    let _: fn(u32) -> Mutex<u32> = parking_lot::const_mutex;
    let _: fn(RawMutex, u32) -> Mutex<u32> = Mutex::const_new;
    let _: for<'a> fn(&'a Mutex<u32>) -> MutexGuard<'a, u32> = Mutex::lock;
    let _: for<'a> fn(&'a Mutex<u32>) -> Option<MutexGuard<'a, u32>> = Mutex::try_lock;
    let _: for<'a> fn(&'a Mutex<u32>, Duration) -> Option<MutexGuard<'a, u32>> = Mutex::try_lock_for;
    let _: for<'a> fn(&'a Mutex<u32>, Instant) -> Option<MutexGuard<'a, u32>> = Mutex::try_lock_until;
    let _: fn(&Mutex<u32>) -> bool = Mutex::is_locked;
    let _: fn(&mut Mutex<u32>) -> &mut u32 = Mutex::get_mut;
    let _: fn(Mutex<u32>) -> u32 = Mutex::into_inner;
    let _: fn(&Mutex<u32>) -> *mut u32 = Mutex::data_ptr;
    let _: for<'a> fn(&'a Mutex<u32>) -> &'a RawMutex = |mutex| unsafe { mutex.raw() };
    // FairMutex.
    let _: fn(u32) -> FairMutex<u32> = FairMutex::new;
    let _: fn(u32) -> FairMutex<u32> = parking_lot::const_fair_mutex;
    let _: for<'a> fn(&'a FairMutex<u32>) -> FairMutexGuard<'a, u32> = FairMutex::lock;
    let _: for<'a> fn(&'a FairMutex<u32>) -> Option<FairMutexGuard<'a, u32>> = FairMutex::try_lock;
    let _: for<'a> fn(&'a FairMutex<u32>, Duration) -> Option<FairMutexGuard<'a, u32>> = FairMutex::try_lock_for;
    let _: for<'a> fn(&'a FairMutex<u32>, Instant) -> Option<FairMutexGuard<'a, u32>> = FairMutex::try_lock_until;
    // ReentrantMutex.
    let _: fn(u32) -> ReentrantMutex<u32> = ReentrantMutex::new;
    let _: fn(u32) -> ReentrantMutex<u32> = parking_lot::const_reentrant_mutex;
    let _: fn(RawMutex, RawThreadId, u32) -> ReentrantMutex<u32> = ReentrantMutex::const_new;
    let _: for<'a> fn(&'a ReentrantMutex<u32>) -> ReentrantMutexGuard<'a, u32> = ReentrantMutex::lock;
    let _: for<'a> fn(&'a ReentrantMutex<u32>) -> Option<ReentrantMutexGuard<'a, u32>> = ReentrantMutex::try_lock;
    let _: for<'a> fn(&'a ReentrantMutex<u32>, Duration) -> Option<ReentrantMutexGuard<'a, u32>> =
        ReentrantMutex::try_lock_for;
    let _: for<'a> fn(&'a ReentrantMutex<u32>, Instant) -> Option<ReentrantMutexGuard<'a, u32>> =
        ReentrantMutex::try_lock_until;
    let _: fn(&ReentrantMutex<u32>) -> bool = ReentrantMutex::is_locked;
    let _: fn(&ReentrantMutex<u32>) -> bool = ReentrantMutex::is_owned_by_current_thread;
    let _: fn(&mut ReentrantMutex<u32>) -> &mut u32 = ReentrantMutex::get_mut;
    let _: fn(ReentrantMutex<u32>) -> u32 = ReentrantMutex::into_inner;
    let _: fn(&RawThreadId) -> NonZeroUsize = <RawThreadId as parking_lot::lock_api::GetThreadId>::nonzero_thread_id;
    // RwLock.
    let _: fn(u32) -> RwLock<u32> = RwLock::new;
    let _: fn(u32) -> RwLock<u32> = parking_lot::const_rwlock;
    let _: for<'a> fn(&'a RwLock<u32>) -> RwLockReadGuard<'a, u32> = RwLock::read;
    let _: for<'a> fn(&'a RwLock<u32>) -> RwLockReadGuard<'a, u32> = RwLock::read_recursive;
    let _: for<'a> fn(&'a RwLock<u32>) -> Option<RwLockReadGuard<'a, u32>> = RwLock::try_read_recursive;
    let _: for<'a> fn(&'a RwLock<u32>, Duration) -> Option<RwLockReadGuard<'a, u32>> = RwLock::try_read_recursive_for;
    let _: for<'a> fn(&'a RwLock<u32>, Instant) -> Option<RwLockReadGuard<'a, u32>> = RwLock::try_read_recursive_until;
    let _: for<'a> fn(&'a RwLock<u32>) -> RwLockWriteGuard<'a, u32> = RwLock::write;
    let _: for<'a> fn(&'a RwLock<u32>) -> RwLockUpgradableReadGuard<'a, u32> = RwLock::upgradable_read;
    let _: for<'a> fn(&'a RwLock<u32>) -> Option<RwLockReadGuard<'a, u32>> = RwLock::try_read;
    let _: for<'a> fn(&'a RwLock<u32>) -> Option<RwLockWriteGuard<'a, u32>> = RwLock::try_write;
    let _: for<'a> fn(&'a RwLock<u32>) -> Option<RwLockUpgradableReadGuard<'a, u32>> = RwLock::try_upgradable_read;
    let _: for<'a> fn(&'a RwLock<u32>, Duration) -> Option<RwLockReadGuard<'a, u32>> = RwLock::try_read_for;
    let _: for<'a> fn(&'a RwLock<u32>, Instant) -> Option<RwLockReadGuard<'a, u32>> = RwLock::try_read_until;
    let _: for<'a> fn(&'a RwLock<u32>, Duration) -> Option<RwLockWriteGuard<'a, u32>> = RwLock::try_write_for;
    let _: for<'a> fn(&'a RwLock<u32>, Instant) -> Option<RwLockWriteGuard<'a, u32>> = RwLock::try_write_until;
    let _: for<'a> fn(&'a RwLock<u32>, Duration) -> Option<RwLockUpgradableReadGuard<'a, u32>> =
        RwLock::try_upgradable_read_for;
    let _: for<'a> fn(&'a RwLock<u32>, Instant) -> Option<RwLockUpgradableReadGuard<'a, u32>> =
        RwLock::try_upgradable_read_until;
    let _: fn(&RwLock<u32>) -> bool = RwLock::is_locked;
    let _: fn(&RwLock<u32>) -> bool = RwLock::is_locked_exclusive;
    // Condvar.
    let _: fn() -> Condvar = Condvar::new;
    let _: fn() -> Condvar = Condvar::default;
    let _: fn(&Condvar) -> bool = Condvar::notify_one;
    let _: fn(&Condvar) -> usize = Condvar::notify_all;
    let _: for<'a, 'b, 'c> fn(&'a Condvar, &'b mut MutexGuard<'c, u32>) = Condvar::wait;
    let _: for<'a, 'b, 'c> fn(&'a Condvar, &'b mut MutexGuard<'c, u32>, Instant) -> WaitTimeoutResult =
        Condvar::wait_until;
    let _: for<'a, 'b, 'c> fn(&'a Condvar, &'b mut MutexGuard<'c, u32>, Duration) -> WaitTimeoutResult =
        Condvar::wait_for;
    let _: for<'a, 'b, 'c> fn(&'a Condvar, &'b mut MutexGuard<'c, u32>, fn(&mut u32) -> bool) = Condvar::wait_while;
    let _: for<'a, 'b, 'c> fn(
        &'a Condvar,
        &'b mut MutexGuard<'c, u32>,
        fn(&mut u32) -> bool,
        Instant,
    ) -> WaitTimeoutResult = Condvar::wait_while_until;
    let _: for<'a, 'b, 'c> fn(
        &'a Condvar,
        &'b mut MutexGuard<'c, u32>,
        fn(&mut u32) -> bool,
        Duration,
    ) -> WaitTimeoutResult = Condvar::wait_while_for;
    let _: fn(WaitTimeoutResult) -> bool = WaitTimeoutResult::timed_out;
    // Once.
    let _: fn() -> Once = Once::new;
    let _: fn() -> Once = Once::default;
    let _: fn(&Once) -> OnceState = Once::state;
    let _: fn(&Once, fn()) = Once::call_once;
    let _: fn(&Once, fn(OnceState)) = Once::call_once_force;
    let _: fn(OnceState) -> bool = OnceState::poisoned;
    let _: fn(OnceState) -> bool = OnceState::done;
    let _ = [
        OnceState::New,
        OnceState::Poisoned,
        OnceState::InProgress,
        OnceState::Done,
    ];
    // The raw types, with the `lock_api` traits `parking_lot`'s implement.
    let _: RawMutex = <RawMutex as parking_lot::lock_api::RawMutex>::INIT;
    let _: RawFairMutex = <RawFairMutex as parking_lot::lock_api::RawMutex>::INIT;
    let _: RawRwLock = <RawRwLock as parking_lot::lock_api::RawRwLock>::INIT;
    let _: RawThreadId = <RawThreadId as parking_lot::lock_api::GetThreadId>::INIT;
    raw_mutex::<RawMutex>();
    raw_mutex::<RawFairMutex>();
    raw_rwlock::<RawRwLock>();
    // The aliases are `lock_api`'s types, over these raw types.
    let _: parking_lot::lock_api::Mutex<RawMutex, u32> = Mutex::new(0);
    let _: parking_lot::lock_api::Mutex<RawFairMutex, u32> = FairMutex::new(0);
    let _: parking_lot::lock_api::ReentrantMutex<RawMutex, RawThreadId, u32> = ReentrantMutex::new(0);
    let _: parking_lot::lock_api::RwLock<RawRwLock, u32> = RwLock::new(0);
    guard_signatures(&());
}

/// The guards' associated functions. A lifetime taken from the caller rather than `for<'a>`: a
/// higher-ranked fn pointer doesn't coerce where the lifetime also appears in a closure argument's
/// type. The argument carries it (an unused lifetime parameter is a clippy error).
#[allow(clippy::type_complexity)]
fn guard_signatures<'a>(_lifetime: &'a ()) {
    type Pair = (u32, u32);

    let _: fn(&MutexGuard<'a, u32>) -> &'a Mutex<u32> = MutexGuard::mutex;
    let _: fn(MutexGuard<'a, Pair>, fn(&mut Pair) -> &mut u32) -> MappedMutexGuard<'a, u32> = MutexGuard::map;
    let _: fn(MutexGuard<'a, u32>) = MutexGuard::unlock_fair;
    let _: fn(&mut MutexGuard<'a, u32>) = MutexGuard::bump;
    let _: fn(MutexGuard<'a, u32>) -> &'a mut u32 = MutexGuard::leak;
    let _: fn(FairMutexGuard<'a, Pair>, fn(&mut Pair) -> &mut u32) -> MappedFairMutexGuard<'a, u32> =
        FairMutexGuard::map;
    let _: fn(ReentrantMutexGuard<'a, Pair>, fn(&Pair) -> &u32) -> MappedReentrantMutexGuard<'a, u32> =
        ReentrantMutexGuard::map;
    let _: fn(ReentrantMutexGuard<'a, u32>) = ReentrantMutexGuard::unlock_fair;
    let _: fn(&mut ReentrantMutexGuard<'a, u32>) = ReentrantMutexGuard::bump;
    let _: fn(RwLockWriteGuard<'a, u32>) -> RwLockReadGuard<'a, u32> = RwLockWriteGuard::downgrade;
    let _: fn(RwLockWriteGuard<'a, u32>) -> RwLockUpgradableReadGuard<'a, u32> =
        RwLockWriteGuard::downgrade_to_upgradable;
    let _: fn(RwLockUpgradableReadGuard<'a, u32>) -> RwLockWriteGuard<'a, u32> = RwLockUpgradableReadGuard::upgrade;
    let _: fn(
        RwLockUpgradableReadGuard<'a, u32>,
    ) -> Result<RwLockWriteGuard<'a, u32>, RwLockUpgradableReadGuard<'a, u32>> = RwLockUpgradableReadGuard::try_upgrade;
    let _: fn(
        RwLockUpgradableReadGuard<'a, u32>,
        Duration,
    ) -> Result<RwLockWriteGuard<'a, u32>, RwLockUpgradableReadGuard<'a, u32>> =
        RwLockUpgradableReadGuard::try_upgrade_for;
    let _: fn(
        RwLockUpgradableReadGuard<'a, u32>,
        Instant,
    ) -> Result<RwLockWriteGuard<'a, u32>, RwLockUpgradableReadGuard<'a, u32>> =
        RwLockUpgradableReadGuard::try_upgrade_until;
    let _: fn(RwLockUpgradableReadGuard<'a, u32>) -> RwLockReadGuard<'a, u32> = RwLockUpgradableReadGuard::downgrade;
    let _: fn(RwLockReadGuard<'a, Pair>, fn(&Pair) -> &u32) -> MappedRwLockReadGuard<'a, u32> = RwLockReadGuard::map;
    let _: fn(RwLockWriteGuard<'a, Pair>, fn(&mut Pair) -> &mut u32) -> MappedRwLockWriteGuard<'a, u32> =
        RwLockWriteGuard::map;
}

fn raw_mutex<R>()
where
    R: parking_lot::lock_api::RawMutex
        + parking_lot::lock_api::RawMutexFair
        + parking_lot::lock_api::RawMutexTimed<Duration = Duration, Instant = Instant>,
{
}

fn raw_rwlock<R>()
where
    R: parking_lot::lock_api::RawRwLock
        + parking_lot::lock_api::RawRwLockFair
        + parking_lot::lock_api::RawRwLockDowngrade
        + parking_lot::lock_api::RawRwLockTimed<Duration = Duration, Instant = Instant>
        + parking_lot::lock_api::RawRwLockRecursive
        + parking_lot::lock_api::RawRwLockRecursiveTimed
        + parking_lot::lock_api::RawRwLockUpgrade
        + parking_lot::lock_api::RawRwLockUpgradeFair
        + parking_lot::lock_api::RawRwLockUpgradeDowngrade
        + parking_lot::lock_api::RawRwLockUpgradeTimed,
{
}

// The `const` constructors, in `const` and `static` items.
const _: Mutex<u32> = parking_lot::const_mutex(0);
const _: Mutex<u32> = Mutex::new(0);
const _: FairMutex<u32> = parking_lot::const_fair_mutex(0);
const _: ReentrantMutex<u32> = parking_lot::const_reentrant_mutex(0);
const _: RwLock<u32> = parking_lot::const_rwlock(0);
const _: Condvar = Condvar::new();
const _: Once = Once::new();

/// Which auto traits each type has, as one line per type, for comparing two builds.
pub fn auto_traits() -> String {
    // A macro, not a generic function: the probe's inherent consts apply only to a concrete type.
    macro_rules! row {
        ($type:ty) => {
            format!(
                "{}: Send={} Sync={} Unpin={} UnwindSafe={} RefUnwindSafe={}",
                stringify!($type),
                Probe::<$type>::SEND,
                Probe::<$type>::SYNC,
                Probe::<$type>::UNPIN,
                Probe::<$type>::UNWIND_SAFE,
                Probe::<$type>::REF_UNWIND_SAFE,
            )
        };
    }
    let rows = [
        row!(Mutex<u32>),
        row!(Mutex<std::cell::Cell<u32>>),
        row!(MutexGuard<'static, u32>),
        row!(MappedMutexGuard<'static, u32>),
        row!(FairMutex<u32>),
        row!(FairMutexGuard<'static, u32>),
        row!(MappedFairMutexGuard<'static, u32>),
        row!(ReentrantMutex<u32>),
        row!(ReentrantMutex<std::cell::Cell<u32>>),
        row!(ReentrantMutexGuard<'static, u32>),
        row!(MappedReentrantMutexGuard<'static, u32>),
        row!(RwLock<u32>),
        row!(RwLockReadGuard<'static, u32>),
        row!(RwLockWriteGuard<'static, u32>),
        row!(RwLockUpgradableReadGuard<'static, u32>),
        row!(MappedRwLockReadGuard<'static, u32>),
        row!(MappedRwLockWriteGuard<'static, u32>),
        row!(Condvar),
        row!(WaitTimeoutResult),
        row!(Once),
        row!(OnceState),
        row!(RawMutex),
        row!(RawFairMutex),
        row!(RawRwLock),
        row!(RawThreadId),
    ];
    rows.join("\n")
}

// Auto traits, detected without failing the build: an inherent const applies when its bound holds,
// and takes precedence over the trait's default otherwise (the `impls` crate's technique).
struct Probe<T: ?Sized>(PhantomData<T>);
trait Fallback {
    const SEND: bool = false;
    const SYNC: bool = false;
    const UNPIN: bool = false;
    const UNWIND_SAFE: bool = false;
    const REF_UNWIND_SAFE: bool = false;
}
impl<T: ?Sized> Fallback for Probe<T> {}
impl<T: ?Sized + Send> Probe<T> {
    const SEND: bool = true;
}
impl<T: ?Sized + Sync> Probe<T> {
    const SYNC: bool = true;
}
impl<T: ?Sized + Unpin> Probe<T> {
    const UNPIN: bool = true;
}
impl<T: ?Sized + UnwindSafe> Probe<T> {
    const UNWIND_SAFE: bool = true;
}
impl<T: ?Sized + RefUnwindSafe> Probe<T> {
    const REF_UNWIND_SAFE: bool = true;
}
