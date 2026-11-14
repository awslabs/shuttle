//! The model has crossbeam-utils's API: the same items with the same signatures, written once and
//! compiled against both crates. Only `Parker::park_deadline`'s `Instant` differs: `std::time`'s
//! for crossbeam, `shuttle-time`'s for the model.
//!
//! This test only has to compile, apart from a run of the primitives in both modes.

macro_rules! signatures {
    ($cu:ident, $atomics:ident, $time:ident, $thread:ident) => {
        #[allow(dead_code, deprecated)]
        pub mod probe {
            use super::{$atomics as atomics, $thread as thread_mod, $time as time};
            use std::fmt::{Debug, Display};
            use std::ops::{Deref, DerefMut};
            use std::sync::{LockResult, TryLockResult};

            // The items of whichever crate this copy of the probe is for. A `use` cannot name them,
            // because a macro's `path` fragment is not a use tree, so they are aliases.
            pub type AtomicCell<T> = $cu::atomic::AtomicCell<T>;
            pub type Backoff = $cu::Backoff;
            pub type CachePadded<T> = $cu::CachePadded<T>;
            pub type Parker = $cu::sync::Parker;
            pub type Unparker = $cu::sync::Unparker;
            pub type ShardedLock<T> = $cu::sync::ShardedLock<T>;
            pub type ShardedLockReadGuard<'a, T> = $cu::sync::ShardedLockReadGuard<'a, T>;
            pub type ShardedLockWriteGuard<'a, T> = $cu::sync::ShardedLockWriteGuard<'a, T>;
            pub type WaitGroup = $cu::sync::WaitGroup;
            pub type Scope<'env> = $cu::thread::Scope<'env>;
            pub type ScopedJoinHandle<'scope, T> = $cu::thread::ScopedJoinHandle<'scope, T>;
            pub type ScopedThreadBuilder<'scope, 'env> = $cu::thread::ScopedThreadBuilder<'scope, 'env>;
            pub type Duration = time::Duration;
            pub type Instant = time::Instant;
            pub type AtomicBool = atomics::AtomicBool;
            pub type AtomicI8 = atomics::AtomicI8;
            pub type AtomicIsize = atomics::AtomicIsize;
            pub type AtomicPtr<T> = atomics::AtomicPtr<T>;
            pub type AtomicU64 = atomics::AtomicU64;
            pub type AtomicUsize = atomics::AtomicUsize;

            fn traits<T: Send + Sync + Debug>() {}

            pub fn cache_padded() {
                let _: fn(u64) -> CachePadded<u64> = CachePadded::new;
                let _: fn(CachePadded<u64>) -> u64 = CachePadded::into_inner;
                let _: for<'g> fn(&'g CachePadded<u64>) -> &'g u64 = <CachePadded<u64> as Deref>::deref;
                let _: for<'g> fn(&'g mut CachePadded<u64>) -> &'g mut u64 = <CachePadded<u64> as DerefMut>::deref_mut;
                let _: CachePadded<u64> = CachePadded::from(1);
                let _: CachePadded<u64> = Default::default();
                traits::<CachePadded<u64>>();
                fn displays<T: Display>() {}
                displays::<CachePadded<u64>>();
            }

            pub fn backoff() {
                let _: fn() -> Backoff = Backoff::new;
                let _: fn(&Backoff) = Backoff::reset;
                let _: fn(&Backoff) = Backoff::spin;
                let _: fn(&Backoff) = Backoff::snooze;
                let _: fn(&Backoff) -> bool = Backoff::is_completed;
                let _: Backoff = Default::default();
                fn debug<T: Debug>() {}
                debug::<Backoff>();
            }

            pub fn atomic_cell() {
                // `new` is a `const fn` in both, which is what a `static` cell needs.
                static _STATIC: AtomicCell<u32> = AtomicCell::new(0);
                let _: fn(u32) -> AtomicCell<u32> = AtomicCell::new;
                let _: fn(AtomicCell<u32>) -> u32 = AtomicCell::into_inner;
                let _: fn() -> bool = AtomicCell::<u32>::is_lock_free;
                let _: fn(&AtomicCell<u32>, u32) = AtomicCell::store;
                let _: fn(&AtomicCell<u32>, u32) -> u32 = AtomicCell::swap;
                let _: fn(&AtomicCell<u32>) -> *mut u32 = AtomicCell::as_ptr;
                let _: fn(&AtomicCell<u32>) -> u32 = AtomicCell::take;
                let _: fn(&AtomicCell<u32>) -> u32 = AtomicCell::load;
                let _: fn(&AtomicCell<u32>, u32, u32) -> u32 = AtomicCell::compare_and_swap;
                let _: fn(&AtomicCell<u32>, u32, u32) -> Result<u32, u32> = AtomicCell::compare_exchange;
                let _: fn(&AtomicCell<u32>, fn(u32) -> Option<u32>) -> Result<u32, u32> = AtomicCell::fetch_update;
                let _: AtomicCell<u32> = AtomicCell::from(1);
                let _: AtomicCell<u32> = Default::default();
                traits::<AtomicCell<u32>>();
                // A cell of a type that is neither `Copy` nor `Eq`.
                let _: fn(&AtomicCell<String>, String) -> String = AtomicCell::swap;

                // The arithmetic of each integer type, and the logic of `bool`.
                let _: fn(&AtomicCell<u8>, u8) -> u8 = AtomicCell::<u8>::fetch_add;
                let _: fn(&AtomicCell<i8>, i8) -> i8 = AtomicCell::<i8>::fetch_sub;
                let _: fn(&AtomicCell<u16>, u16) -> u16 = AtomicCell::<u16>::fetch_and;
                let _: fn(&AtomicCell<i16>, i16) -> i16 = AtomicCell::<i16>::fetch_nand;
                let _: fn(&AtomicCell<u32>, u32) -> u32 = AtomicCell::<u32>::fetch_or;
                let _: fn(&AtomicCell<i32>, i32) -> i32 = AtomicCell::<i32>::fetch_xor;
                let _: fn(&AtomicCell<u64>, u64) -> u64 = AtomicCell::<u64>::fetch_max;
                let _: fn(&AtomicCell<i64>, i64) -> i64 = AtomicCell::<i64>::fetch_min;
                let _: fn(&AtomicCell<u128>, u128) -> u128 = AtomicCell::<u128>::fetch_add;
                let _: fn(&AtomicCell<i128>, i128) -> i128 = AtomicCell::<i128>::fetch_sub;
                let _: fn(&AtomicCell<usize>, usize) -> usize = AtomicCell::<usize>::fetch_add;
                let _: fn(&AtomicCell<isize>, isize) -> isize = AtomicCell::<isize>::fetch_sub;
                let _: fn(&AtomicCell<bool>, bool) -> bool = AtomicCell::<bool>::fetch_and;
                let _: fn(&AtomicCell<bool>, bool) -> bool = AtomicCell::<bool>::fetch_nand;
                let _: fn(&AtomicCell<bool>, bool) -> bool = AtomicCell::<bool>::fetch_or;
                let _: fn(&AtomicCell<bool>, bool) -> bool = AtomicCell::<bool>::fetch_xor;
            }

            pub fn atomic_consume() {
                let _: fn(&AtomicBool) -> bool = <AtomicBool as $cu::atomic::AtomicConsume>::load_consume;
                let _: fn(&AtomicUsize) -> usize = <AtomicUsize as $cu::atomic::AtomicConsume>::load_consume;
                let _: fn(&AtomicIsize) -> isize = <AtomicIsize as $cu::atomic::AtomicConsume>::load_consume;
                let _: fn(&AtomicI8) -> i8 = <AtomicI8 as $cu::atomic::AtomicConsume>::load_consume;
                let _: fn(&AtomicU64) -> u64 = <AtomicU64 as $cu::atomic::AtomicConsume>::load_consume;
                let _: fn(&AtomicPtr<u8>) -> *mut u8 = <AtomicPtr<u8> as $cu::atomic::AtomicConsume>::load_consume;
            }

            pub fn sharded_lock() {
                let _: fn(u32) -> ShardedLock<u32> = ShardedLock::new;
                let _: fn(ShardedLock<u32>) -> LockResult<u32> = ShardedLock::into_inner;
                let _: fn(&ShardedLock<u32>) -> bool = ShardedLock::is_poisoned;
                let _: for<'a> fn(&'a mut ShardedLock<u32>) -> LockResult<&'a mut u32> = ShardedLock::get_mut;
                let _: for<'a> fn(&'a ShardedLock<u32>) -> TryLockResult<ShardedLockReadGuard<'a, u32>> =
                    ShardedLock::try_read;
                let _: for<'a> fn(&'a ShardedLock<u32>) -> LockResult<ShardedLockReadGuard<'a, u32>> =
                    ShardedLock::read;
                let _: for<'a> fn(&'a ShardedLock<u32>) -> TryLockResult<ShardedLockWriteGuard<'a, u32>> =
                    ShardedLock::try_write;
                let _: for<'a> fn(&'a ShardedLock<u32>) -> LockResult<ShardedLockWriteGuard<'a, u32>> =
                    ShardedLock::write;
                let _: ShardedLock<u32> = ShardedLock::from(1);
                let _: ShardedLock<u32> = Default::default();
                traits::<ShardedLock<u32>>();
                let _: for<'g> fn(&'g ShardedLockReadGuard<'static, u32>) -> &'g u32 =
                    <ShardedLockReadGuard<'static, u32> as Deref>::deref;
                let _: for<'g> fn(&'g mut ShardedLockWriteGuard<'static, u32>) -> &'g mut u32 =
                    <ShardedLockWriteGuard<'static, u32> as DerefMut>::deref_mut;
                fn shows<T: Debug + Display>() {}
                shows::<ShardedLockReadGuard<'static, u32>>();
                shows::<ShardedLockWriteGuard<'static, u32>>();
                // A lock of an unsized value.
                let _: fn(&ShardedLock<[u32]>) -> bool = ShardedLock::is_poisoned;
            }

            pub fn wait_group() {
                let _: fn() -> WaitGroup = WaitGroup::new;
                let _: fn(WaitGroup) = WaitGroup::wait;
                let _: fn(&WaitGroup) -> WaitGroup = WaitGroup::clone;
                let _: WaitGroup = Default::default();
                traits::<WaitGroup>();
            }

            pub fn parker() {
                let _: fn() -> Parker = Parker::new;
                let _: fn(&Parker) = Parker::park;
                let _: fn(&Parker, Duration) = Parker::park_timeout;
                let _: fn(&Parker, Instant) = Parker::park_deadline;
                let _: fn(&Parker) -> &Unparker = Parker::unparker;
                let _: fn(Parker) -> *const () = Parker::into_raw;
                let _: unsafe fn(*const ()) -> Parker = Parker::from_raw;
                let _: fn(&Unparker) = Unparker::unpark;
                let _: fn(Unparker) -> *const () = Unparker::into_raw;
                let _: unsafe fn(*const ()) -> Unparker = Unparker::from_raw;
                let _: fn(&Unparker) -> Unparker = Unparker::clone;
                let _: Parker = Default::default();
                fn send<T: Send>() {}
                send::<Parker>();
                traits::<Unparker>();
                fn debug<T: Debug>() {}
                debug::<Parker>();
            }

            pub fn scoped_threads() {
                let _: fn(fn(&Scope<'_>) -> u8) -> std::thread::Result<u8> = $cu::thread::scope;
                fn body(s: &Scope<'_>) -> usize {
                    let handle: ScopedJoinHandle<'_, usize> = s.spawn(|_| 1usize);
                    let builder: ScopedThreadBuilder<'_, '_> = s.builder();
                    let named = builder
                        .name("name".to_string())
                        .stack_size(1 << 16)
                        .spawn(|_| 2usize)
                        .expect("spawning a scoped thread");
                    let _: &thread_mod::Thread = named.thread();
                    let _: std::thread::Result<usize> = named.join();
                    handle.join().expect("joining a scoped thread")
                }
                let _: fn(fn(&Scope<'_>) -> usize) -> std::thread::Result<usize> = $cu::thread::scope;
                let _ = body;
                fn debug<T: Debug>() {}
                debug::<ScopedJoinHandle<'static, u8>>();
            }
        }
    };
}

mod real {
    pub(crate) use std::sync::atomic as atomics;
    pub(crate) use std::thread;
    pub(crate) use std::time;

    signatures!(crossbeam_utils, atomics, time, thread);
}

mod model {
    pub(crate) use shuttle::sync::atomic as atomics;
    pub(crate) use shuttle::thread;
    pub(crate) use shuttle_time_inner::time;

    signatures!(shuttle_crossbeam_utils_inner, atomics, time, thread);
}

#[test]
fn the_probe_runs_on_crossbeam() {
    real::probe::cache_padded();
    real::probe::backoff();
    real::probe::atomic_cell();
}

#[test]
fn the_probe_runs_under_shuttle() {
    shuttle::check_random(
        || {
            model::probe::cache_padded();
            model::probe::backoff();
            model::probe::atomic_cell();
        },
        10,
    );
}
