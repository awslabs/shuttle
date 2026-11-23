//! Tests for the checked `UnsafeCell` of `shuttle::cell`, with its data race checks.

use shuttle::cell::{Ptr, UnsafeCell};
use shuttle::scheduler::{PctScheduler, RandomScheduler};
use shuttle::sync::atomic::{AtomicUsize, Ordering};
use shuttle::sync::{Arc, Mutex};
use shuttle::{check_dfs, future, thread, Config, Runner};
use std::panic;
use test_log::test;

const SEED: u64 = 0x5eed;
const ITERATIONS: usize = 1_000;

/// Runs `f` under the random scheduler with `config` until an execution fails, and returns the
/// failure's panic message.
fn failure_with<F>(config: Config, f: F) -> String
where
    F: Fn() + Send + Sync + 'static,
{
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        Runner::new(RandomScheduler::new_from_seed(SEED, ITERATIONS), config).run(f)
    }));
    let payload = result.expect_err("some execution should fail");
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        message.to_string()
    } else {
        panic!("the failure's payload isn't a message")
    }
}

fn failure<F>(f: F) -> String
where
    F: Fn() + Send + Sync + 'static,
{
    failure_with(Config::new(), f)
}

/// Runs `f` under the random and PCT schedulers with `config`. Every execution must pass.
fn passes_with<F>(config: Config, f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let f = Arc::new(f);
    let g = Arc::clone(&f);
    Runner::new(RandomScheduler::new_from_seed(SEED, ITERATIONS), config.clone()).run(move || g());
    Runner::new(PctScheduler::new_from_seed(SEED, 3, ITERATIONS), config).run(move || f());
}

/// Runs `f` under the random scheduler only, for tests whose only scheduling points are spawns and
/// joins, which the PCT scheduler refuses. Every execution must pass.
fn passes_random<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    Runner::new(RandomScheduler::new_from_seed(SEED, ITERATIONS), Config::new()).run(f);
}

/// A value that tasks share, with access only through the checked cell.
struct Shared<T>(UnsafeCell<T>);

// SAFETY: the tests are about the accesses this allows.
unsafe impl<T: Send> Sync for Shared<T> {}

/// Two fields, at bytes 0..4 and 4..8.
#[repr(C)]
struct Pair {
    a: u32,
    b: u32,
}

// Each byte is a location of its own, as in Miri. Two tasks that write different fields without
// synchronizing don't race.
#[test]
fn different_fields_dont_race() {
    check_dfs(
        || {
            let cell = Arc::new(Shared(UnsafeCell::new(Pair { a: 0, b: 0 })));
            let other = Arc::clone(&cell);
            let t = thread::spawn(move || unsafe { other.0.get().project(|p| &raw mut (*p).a).write(1) });
            unsafe { cell.0.get().project(|p| &raw mut (*p).b).write(2) };
            t.join().unwrap();
        },
        None,
    );
}

#[test]
fn use_after_free() {
    let report = failure(|| {
        let cell = UnsafeCell::new(0u32);
        let p = cell.get();
        drop(cell);
        let _val = unsafe { p.read() };
    });
    assert!(report.contains("is a use after free"), "{report}");
}

#[test]
fn out_of_bounds() {
    let report = failure(|| {
        let cell = UnsafeCell::new([0u8; 4]);
        let _val = unsafe { cell.get().cast::<u8>().add(4).read() };
    });
    assert!(report.contains("is out of bounds"), "{report}");
}

struct Counter {
    lock: Mutex<()>,
    value: UnsafeCell<u64>,
}

// SAFETY: `value` is accessed under `lock`, except in the buggy methods that the tests are about.
unsafe impl Sync for Counter {}

impl Counter {
    fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            value: UnsafeCell::new(0),
        }
    }

    fn increment(&self) {
        let _guard = self.lock.lock().unwrap();
        let mut value = unsafe { self.value.get().as_mut() };
        *value += 1;
    }

    /// Makes the reference before it takes the lock.
    fn increment_reference_first(&self) {
        let mut value = unsafe { self.value.get().as_mut() };
        let _guard = self.lock.lock().unwrap();
        *value += 1;
    }

    fn get(&self) -> u64 {
        let _guard = self.lock.lock().unwrap();
        unsafe { self.value.get().read() }
    }
}

#[test]
fn counter_under_a_lock_passes() {
    let test = || {
        let counter = Arc::new(Counter::new());
        let other = Arc::clone(&counter);
        let t = thread::spawn(move || other.increment());
        counter.increment();
        t.join().unwrap();
        assert_eq!(counter.get(), 2);
    };
    passes_with(Config::new(), test);
    check_dfs(test, None);
}

// The reference is made before the lock. Every write is under the lock, but making the reference
// reads the value, and that read races with the other task's write.
#[test]
fn reference_made_before_the_lock_races() {
    let report = failure(|| {
        let counter = Arc::new(Counter::new());
        let other = Arc::clone(&counter);
        let t = thread::spawn(move || other.increment_reference_first());
        counter.increment_reference_first();
        t.join().unwrap();
    });
    assert!(report.contains("Data race"), "{report}");
    assert!(report.contains("reference creation"), "{report}");
}

#[test]
fn unsynchronized_write_and_read_race() {
    let report = failure(|| {
        let cell = Arc::new(Shared(UnsafeCell::new(0u32)));
        let other = Arc::clone(&cell);
        let t = thread::spawn(move || unsafe { other.0.get().write(1) });
        let _val = unsafe { cell.0.get().read() };
        t.join().unwrap();
    });
    assert!(report.contains("Data race"), "{report}");
}

// A write races with each earlier read that doesn't happen before it, not only with the last one.
// The other task reads first, and this task waits for that through a std atomic, which Shuttle
// doesn't see. Then this task reads and writes.
#[test]
fn write_races_with_an_earlier_read_of_another_task() {
    let report = failure(|| {
        let cell = Arc::new(Shared(UnsafeCell::new(0u32)));
        let read = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (cell, read) = (Arc::clone(&cell), Arc::clone(&read));
            thread::spawn(move || {
                let _val = unsafe { cell.0.get().read() };
                read.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        };
        while !read.load(std::sync::atomic::Ordering::SeqCst) {
            thread::yield_now();
        }
        let _val = unsafe { cell.0.get().read() };
        unsafe { cell.0.get().write(1) };
        reader.join().unwrap();
    });
    assert!(report.contains("Data race"), "{report}");
    assert!(report.contains("an earlier read in task 1"), "{report}");
}

// Miri's tests/fail/both_borrows/retag_data_race_write.rs: making a reference reads, for the race
// detector.
#[test]
fn reference_creation_races_with_a_write() {
    let report = failure(|| {
        let cell = Arc::new(Shared(UnsafeCell::new(0u8)));
        let (a, b) = (Arc::clone(&cell), Arc::clone(&cell));
        let t1 = thread::spawn(move || {
            let _r = unsafe { a.0.get().as_mut() };
        });
        let t2 = thread::spawn(move || unsafe { b.0.get().write(5) });
        t1.join().unwrap();
        t2.join().unwrap();
    });
    assert!(report.contains("Data race"), "{report}");
    assert!(report.contains("reference creation"), "{report}");
}

// A write made after the Release store isn't published by it, so it races with the read of a task
// that acquired the store. The reader waits for the second write through a std atomic, which
// Shuttle doesn't see, so that the read always comes after both writes.
#[test]
fn write_after_the_release_races() {
    let report = failure(|| {
        let cell = Arc::new(Shared(UnsafeCell::new(0u32)));
        let flag = Arc::new(AtomicUsize::new(0));
        let written = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let (cell, flag, written) = (Arc::clone(&cell), Arc::clone(&flag), Arc::clone(&written));
            thread::spawn(move || {
                unsafe { cell.0.get().write(1) };
                flag.store(1, Ordering::Release);
                unsafe { cell.0.get().write(2) };
                written.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        };
        while !written.load(std::sync::atomic::Ordering::SeqCst) {
            thread::yield_now();
        }
        assert_eq!(flag.load(Ordering::Acquire), 1);
        let _val = unsafe { cell.0.get().read() };
        writer.join().unwrap();
    });
    assert!(report.contains("Data race"), "{report}");
    assert!(report.contains("a read in task 0"), "{report}");
}

// Awaiting a future's `JoinHandle` orders the future's accesses before the awaiting task's.
#[test]
fn awaiting_a_join_handle_hands_over() {
    passes_random(|| {
        let cell = Arc::new(Shared(UnsafeCell::new(0u32)));
        let other = Arc::clone(&cell);
        let handle = future::spawn(async move { unsafe { other.0.get().write(1) } });
        future::block_on(handle).unwrap();
        assert_eq!(unsafe { cell.0.get().read() }, 1);
    });
}

// The end of `thread::scope` orders its threads' accesses before what follows it.
#[test]
fn the_end_of_a_scope_hands_over() {
    passes_random(|| {
        let cell = Shared(UnsafeCell::new(0u32));
        let shared = &cell;
        thread::scope(|s| {
            s.spawn(move || unsafe { shared.0.get().write(1) });
        });
        assert_eq!(unsafe { cell.0.get().read() }, 1);
    });
}

// A `lazy_static` that another task initialized hands its value over: the reader inherits the clock
// of the initializer, as with the real `lazy_static`.
#[test]
fn a_lazy_static_cell_hands_over() {
    shuttle::lazy_static! {
        static ref SLOT: Shared<u32> = Shared(UnsafeCell::new(7));
    }

    passes_random(|| {
        let readers: Vec<_> = (0..2)
            .map(|_| thread::spawn(|| assert_eq!(unsafe { SLOT.0.get().read() }, 7)))
            .collect();
        for reader in readers {
            reader.join().unwrap();
        }
    });
}

struct ResetOnDrop {
    slot: UnsafeCell<u32>,
}

// SAFETY: only the main thread writes `slot`, before the last drop.
unsafe impl Sync for ResetOnDrop {}

impl Drop for ResetOnDrop {
    fn drop(&mut self) {
        *self.slot.get_mut() = 0;
    }
}

// The last drop of a std `Arc` comes after the other drops, which Shuttle doesn't see. A `Drop`
// impl that reaches the cell through `get_mut` isn't checked for a race, so this passes.
#[test]
fn a_drop_after_the_last_arc_uses_get_mut() {
    passes_random(|| {
        let value = Arc::new(ResetOnDrop {
            slot: UnsafeCell::new(0),
        });
        let other = Arc::clone(&value);
        let t = thread::spawn(move || drop(other));
        unsafe { value.slot.get().write(1) };
        drop(value);
        t.join().unwrap();
    });
}

struct ReadOnDrop(Arc<Shared<u32>>);

impl Drop for ReadOnDrop {
    fn drop(&mut self) {
        let _val = unsafe { self.0 .0.get().read() };
    }
}

// Teardown drops the future of a detached task that never finishes. Its read isn't checked for a
// race with the main thread's write, because nothing orders the drop.
#[test]
fn teardown_drops_arent_checked_for_races() {
    passes_random(|| {
        let cell = Arc::new(Shared(UnsafeCell::new(0u32)));
        let read_on_drop = ReadOnDrop(Arc::clone(&cell));
        let _detached = future::spawn(async move {
            let _read_on_drop = read_on_drop;
            std::future::pending::<()>().await;
        });
        thread::yield_now();
        unsafe { cell.0.get().write(1) };
    });
}

/// Reads through a pointer when it is dropped.
struct ReadThroughOnDrop(Ptr<u32>);

impl Drop for ReadThroughOnDrop {
    fn drop(&mut self) {
        let _val = unsafe { self.0.read() };
    }
}

/// A pointer to a cell that is already dropped.
fn freed_ptr() -> Ptr<u32> {
    let cell = UnsafeCell::new(0u32);
    let p = cell.get();
    drop(cell);
    p
}

// A second violation while the first unwinds doesn't panic again, which would abort the process.
// The test fails with the first report.
#[test]
fn a_violation_while_unwinding_keeps_the_first_report() {
    let report = failure(|| {
        let _read_on_drop = ReadThroughOnDrop(freed_ptr());
        let cell = UnsafeCell::new([0u8; 4]);
        let _val = unsafe { cell.get().cast::<u8>().add(4).read() };
    });
    assert!(report.contains("is out of bounds"), "{report}");
    assert!(!report.contains("use after free"), "{report}");
}

// A violation in a `Drop` while a panic unwinds can't panic, and the code then catches the first
// panic. The execution still fails, with the violation's report, when it ends.
#[test]
fn a_violation_that_a_caught_panic_hides_fails_the_test() {
    let report = failure(|| {
        let p = freed_ptr();
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let _read_on_drop = ReadThroughOnDrop(p);
            panic!("the code's own panic");
        }));
        assert!(result.is_err());
    });
    assert!(report.contains("is a use after free"), "{report}");
}

// loom's `with` and `with_mut` count as a read and a write.
#[test]
fn loom_api_races() {
    let report = failure(|| {
        let cell = Arc::new(Shared(UnsafeCell::new(0u32)));
        let other = Arc::clone(&cell);
        let t = thread::spawn(move || other.0.with_mut(|p| unsafe { *p = 1 }));
        cell.0.with(|p| unsafe { *p });
        t.join().unwrap();
    });
    assert!(report.contains("Data race"), "{report}");
}

/// A single-producer single-consumer ring of two slots. `publish` is the ordering of the
/// producer's store of `tail`, and `free` the consumer's store of `head`. Both must be Release.
struct Ring {
    slots: [UnsafeCell<u64>; 2],
    head: AtomicUsize,
    tail: AtomicUsize,
    publish: Ordering,
    free: Ordering,
}

// SAFETY: a slot is written only by the producer and read only by the consumer, and the indices
// hand it over, if the orderings are right.
unsafe impl Sync for Ring {}

impl Ring {
    fn new(publish: Ordering, free: Ordering) -> Self {
        Self {
            slots: [UnsafeCell::new(0), UnsafeCell::new(0)],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            publish,
            free,
        }
    }

    fn push(&self, value: u64) -> bool {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail - head == self.slots.len() {
            return false;
        }
        unsafe { self.slots[tail % self.slots.len()].get().write(value) };
        self.tail.store(tail + 1, self.publish);
        true
    }

    fn pop(&self) -> Option<u64> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let value = unsafe { self.slots[head % self.slots.len()].get().read() };
        self.head.store(head + 1, self.free);
        Some(value)
    }
}

/// The producer pushes 1 to 4, and the consumer pops them, so the ring wraps around.
fn ring_test(publish: Ordering, free: Ordering) -> impl Fn() + Send + Sync + 'static {
    move || {
        let ring = Arc::new(Ring::new(publish, free));
        let producer = {
            let ring = Arc::clone(&ring);
            thread::spawn(move || {
                for value in 1..=4 {
                    while !ring.push(value) {
                        thread::yield_now();
                    }
                }
            })
        };
        for value in 1..=4 {
            loop {
                if let Some(popped) = ring.pop() {
                    assert_eq!(popped, value);
                    break;
                }
                thread::yield_now();
            }
        }
        producer.join().unwrap();
    }
}

#[test]
fn ring_with_release_stores_passes() {
    passes_with(Config::new(), ring_test(Ordering::Release, Ordering::Release));
}
