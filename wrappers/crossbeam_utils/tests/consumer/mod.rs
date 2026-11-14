//! Code written against crossbeam-utils, as a crate that depends on `shuttle-crossbeam-utils` under
//! the name `crossbeam_utils` would have it. The same source is compiled against the wrapper
//! without the `shuttle` feature, where it is crossbeam-utils, against the wrapper with the
//! feature, and against `shuttle-crossbeam-utils-inner`, which is what the wrapper is under
//! `shuttle`. The test that includes it provides `crate::crossbeam_utils`, `crate::thread` (std's
//! or Shuttle's), `crate::sync` (`std::sync` or Shuttle's) and `crate::time`.
//!
//! The checks hold for real threads and time as well as under Shuttle, so they assume nothing about
//! how long anything takes, except that a park with a short timeout returns.

use crate::crossbeam_utils::atomic::{AtomicCell, AtomicConsume};
use crate::crossbeam_utils::sync::{Parker, ShardedLock, WaitGroup};
use crate::crossbeam_utils::{Backoff, CachePadded, thread as cb_thread};
use crate::sync::Arc;
use crate::sync::atomic::{AtomicUsize, Ordering};
use crate::thread;
use crate::time::Duration;

/// Exercises the crossbeam-utils API.
pub fn exercise() {
    cache_padded();
    backoff();
    atomic_cell();
    atomic_consume();
    sharded_lock();
    wait_group();
    parker();
    scoped_threads();
}

fn cache_padded() {
    let padded = CachePadded::new(7u64);
    assert_eq!(*padded, 7);
    assert!(size_of_val(&padded) >= 8);
    let mut padded = CachePadded::new([0u8; 4]);
    padded[1] = 3;
    assert_eq!(padded.into_inner(), [0, 3, 0, 0]);
}

fn backoff() {
    let backoff = Backoff::new();
    assert!(!backoff.is_completed());
    // The step counter completes after the same number of calls in both modes.
    for _ in 0..7 {
        backoff.spin();
    }
    assert!(!backoff.is_completed());
    for _ in 0..11 {
        backoff.snooze();
    }
    assert!(backoff.is_completed());
    backoff.reset();
    assert!(!backoff.is_completed());
    let _ = format!("{:?}", Backoff::default());
}

fn atomic_cell() {
    let cell = AtomicCell::new(7u32);
    assert_eq!(cell.load(), 7);
    cell.store(8);
    assert_eq!(cell.swap(9), 8);
    assert_eq!(cell.compare_exchange(9, 10), Ok(9));
    assert_eq!(cell.compare_exchange(9, 11), Err(10));
    assert_eq!(cell.fetch_add(5), 10);
    assert_eq!(cell.fetch_sub(1), 15);
    assert_eq!(cell.fetch_max(100), 14);
    assert_eq!(cell.fetch_min(1), 100);
    assert_eq!(cell.fetch_and(0b11), 1);
    assert_eq!(cell.fetch_or(0b100), 1);
    assert_eq!(cell.fetch_xor(0b1), 0b101);
    assert_eq!(cell.fetch_nand(0), 0b100);
    assert_eq!(cell.fetch_update(|v| Some(v.wrapping_add(1))), Ok(u32::MAX));
    assert_eq!(cell.fetch_update(|_| None), Err(0));
    assert_eq!(cell.take(), 0);
    assert_eq!(cell.into_inner(), 0);

    let flag = AtomicCell::new(true);
    assert!(flag.fetch_and(false));
    assert!(!flag.fetch_or(true));
    assert!(flag.fetch_xor(true));
    assert!(!flag.fetch_nand(true));
    assert!(flag.load());

    // A cell of a type too large for an atomic instruction works the same.
    let big = AtomicCell::new([0u8; 32]);
    big.store([1u8; 32]);
    assert_eq!(big.swap([2u8; 32]), [1u8; 32]);
    assert!(!AtomicCell::<[u8; 32]>::is_lock_free());
    assert!(AtomicCell::<usize>::is_lock_free());
    assert!(AtomicCell::<()>::is_lock_free());

    // A cell of a type that is not `Copy`.
    let owned = AtomicCell::new(String::from("first"));
    assert_eq!(owned.swap(String::from("second")), "first");
    owned.store(String::from("third"));
    assert_eq!(owned.take(), "third");
    let from = AtomicCell::from(1u8);
    // Safety: nothing else reaches the cell, and the pointer is its value's.
    assert_eq!(unsafe { *from.as_ptr() }, 1);
    let _ = format!("{:?}", AtomicCell::<u8>::default());
}

fn atomic_consume() {
    let value = AtomicUsize::new(5);
    assert_eq!(value.load_consume(), 5);
    value.store(6, Ordering::SeqCst);
    assert_eq!(value.load_consume(), 6);
}

fn sharded_lock() {
    let lock = ShardedLock::new(vec![1u8]);
    assert!(!lock.is_poisoned());
    {
        let read = lock.read().unwrap();
        assert_eq!(*read, vec![1]);
        // No writer can get in while a reader holds the lock. (A second read in the same thread is
        // left to the threads below: a recursive read may panic, as `std::sync::RwLock`'s docs say.)
        assert!(lock.try_write().is_err());
    }
    {
        let write = lock.try_write().unwrap();
        assert_eq!(write.len(), 1);
        assert!(lock.try_read().is_err());
    }
    {
        let mut write = lock.write().unwrap();
        write.push(2);
    }
    assert_eq!(*lock.read().unwrap(), vec![1, 2]);
    let _ = format!("{:?} {:?}", lock, lock.read().unwrap());

    let mut lock = lock;
    lock.get_mut().unwrap().push(3);
    assert_eq!(lock.into_inner().unwrap(), vec![1, 2, 3]);

    // Shared between threads: two writers, and two readers that can hold the lock at once.
    let lock = Arc::new(ShardedLock::new(0u32));
    let writers = (0..2)
        .map(|_| {
            let lock = Arc::clone(&lock);
            thread::spawn(move || *lock.write().unwrap() += 1)
        })
        .collect::<Vec<_>>();
    for writer in writers {
        writer.join().unwrap();
    }
    assert_eq!(*lock.read().unwrap(), 2);
    let readers = (0..2)
        .map(|_| {
            let lock = Arc::clone(&lock);
            thread::spawn(move || {
                let guard = lock.read().unwrap();
                thread::yield_now();
                *guard
            })
        })
        .collect::<Vec<_>>();
    for reader in readers {
        assert_eq!(reader.join().unwrap(), 2);
    }
    let _: ShardedLock<u8> = ShardedLock::from(1);
    let _: ShardedLock<u8> = Default::default();
}

fn wait_group() {
    let wg = WaitGroup::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let workers = (0..3)
        .map(|_| {
            let (wg, counter) = (wg.clone(), Arc::clone(&counter));
            thread::spawn(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                // Dropping the clone releases this worker's reference.
                drop(wg);
            })
        })
        .collect::<Vec<_>>();
    let _ = format!("{wg:?}");
    wg.wait();
    // Every worker has run by the time the wait returns.
    assert_eq!(counter.load(Ordering::SeqCst), 3);
    for worker in workers {
        worker.join().unwrap();
    }

    // A worker that waits as well: all of them get past the wait together.
    let wg = WaitGroup::new();
    let others = (0..2)
        .map(|_| {
            let wg = wg.clone();
            thread::spawn(move || wg.wait())
        })
        .collect::<Vec<_>>();
    wg.wait();
    for other in others {
        other.join().unwrap();
    }
}

fn parker() {
    let parker = Parker::new();
    let unparker = parker.unparker().clone();
    // The token is available before the park, so the park returns at once.
    unparker.unpark();
    parker.park();
    // A second unpark while the token is there changes nothing.
    unparker.unpark();
    unparker.unpark();
    parker.park();

    // Unparked from another thread.
    let worker = {
        let unparker = unparker.clone();
        thread::spawn(move || unparker.unpark())
    };
    parker.park();
    worker.join().unwrap();

    // With no token, a timed park returns without one.
    parker.park_timeout(Duration::from_millis(1));
    let _ = format!("{parker:?} {unparker:?}");

    // Through a raw pointer and back.
    let raw = Parker::into_raw(parker);
    let parker = unsafe { Parker::from_raw(raw) };
    unparker.unpark();
    parker.park();
    let raw = crate::crossbeam_utils::sync::Unparker::into_raw(unparker);
    drop(unsafe { crate::crossbeam_utils::sync::Unparker::from_raw(raw) });
}

fn scoped_threads() {
    let mut greeting = String::from("hello");
    let people = ["a".to_string(), "b".to_string()];

    let total = cb_thread::scope(|s| {
        // A borrow of the environment, which the scope keeps alive.
        let handles = people
            .iter()
            .map(|person| s.spawn(move |_| person.len()))
            .collect::<Vec<_>>();
        // A child that spawns into the same scope, through the `Scope` it is given.
        let nested = s.spawn(|s| s.spawn(|_| 40).join().unwrap() + 2);
        // A named thread through the builder.
        let named = s
            .builder()
            .name("worker".to_string())
            .stack_size(1 << 16)
            .spawn(|_| 0usize)
            .unwrap();
        assert!(named.thread().name().is_some_and(|name| name == "worker"));
        let mut total = nested.join().unwrap() + named.join().unwrap();
        for handle in handles {
            total += handle.join().unwrap();
        }
        // A thread nobody joins: the scope waits for it.
        s.spawn(|_| ());
        total
    })
    .unwrap();
    assert_eq!(total, 44);

    // The borrow is free again once the scope has ended.
    greeting.push('!');
    assert_eq!(greeting, "hello!");

    // A scope that mutates a borrow.
    let mut counter = 0u32;
    cb_thread::scope(|s| {
        s.spawn(|_| {});
        counter += 1;
    })
    .unwrap();
    assert_eq!(counter, 1);
}
