//! Tests for the weak-memory emulation that `Config::weak_memory` turns on. Most are litmus tests:
//! each one checks that the emulation produces an outcome that the orderings allow, or that it never
//! produces one they forbid.

use crate::basic::clocks::check_clock;
use shuttle::scheduler::{
    DfsScheduler, PctScheduler, RandomScheduler, ReplayScheduler, Schedule, Scheduler, Task, TaskId,
};
use shuttle::sync::atomic::{fence, AtomicBool, AtomicUsize, Ordering};
use shuttle::{thread, Config, FailurePersistence, Runner};
use std::collections::HashSet;
use std::fmt::Debug;
use std::hash::Hash;
use std::panic;
use std::sync::{Arc, Mutex};
use test_log::test;

use Ordering::{AcqRel, Acquire, Relaxed, Release, SeqCst};

const SEED: u64 = 0x5eed;
const ITERATIONS: usize = 2_000;

fn weak_memory_config() -> Config {
    let mut config = Config::new();
    config.weak_memory = true;
    config
}

/// Runs `f` with weak-memory emulation under `scheduler`, and returns the outcomes it saw.
fn outcomes<T, F, S>(scheduler: S, f: F) -> HashSet<T>
where
    T: Eq + Hash + Clone + Send + 'static,
    F: Fn() -> T + Send + Sync + 'static,
    S: Scheduler + 'static,
{
    let seen = Arc::new(Mutex::new(HashSet::new()));
    let seen_by_test = Arc::clone(&seen);
    Runner::new(scheduler, weak_memory_config()).run(move || {
        let outcome = f();
        seen_by_test.lock().unwrap().insert(outcome);
    });
    let seen = seen.lock().unwrap().clone();
    seen
}

/// The outcomes of `f` under the random and PCT schedulers.
fn all_outcomes<T, F>(f: F) -> HashSet<T>
where
    T: Eq + Hash + Clone + Send + Debug + 'static,
    F: Fn() -> T + Send + Sync + 'static,
{
    let f = Arc::new(f);
    let mut seen = {
        let f = Arc::clone(&f);
        outcomes(RandomScheduler::new_from_seed(SEED, ITERATIONS), move || f())
    };
    seen.extend(outcomes(PctScheduler::new_from_seed(SEED, 3, ITERATIONS), move || f()));
    seen
}

// Store buffering: each thread stores to one location, then loads the other.

fn store_buffering(store: Ordering, load: Ordering, fences: bool) -> (usize, usize) {
    let x = Arc::new(AtomicUsize::new(0));
    let y = Arc::new(AtomicUsize::new(0));
    let other = {
        let (x, y) = (Arc::clone(&x), Arc::clone(&y));
        thread::spawn(move || {
            x.store(1, store);
            if fences {
                fence(SeqCst);
            }
            y.load(load)
        })
    };
    y.store(1, store);
    if fences {
        fence(SeqCst);
    }
    let b = x.load(load);
    (other.join().unwrap(), b)
}

#[test]
fn store_buffering_relaxed_can_read_both_old_values() {
    let seen = all_outcomes(|| store_buffering(Relaxed, Relaxed, false));
    assert!(seen.contains(&(0, 0)), "{seen:?}");
}

#[test]
fn store_buffering_release_acquire_can_read_both_old_values() {
    let seen = all_outcomes(|| store_buffering(Release, Acquire, false));
    assert!(seen.contains(&(0, 0)), "{seen:?}");
}

#[test]
fn store_buffering_seqcst_never_reads_both_old_values() {
    let seen = all_outcomes(|| store_buffering(SeqCst, SeqCst, false));
    assert_eq!(seen, HashSet::from([(0, 1), (1, 0), (1, 1)]));
}

#[test]
fn store_buffering_with_seqcst_fences_never_reads_both_old_values() {
    let seen = all_outcomes(|| store_buffering(Relaxed, Relaxed, true));
    assert!(!seen.contains(&(0, 0)), "{seen:?}");
}

// Message passing: one thread stores data and then a flag. The other loads the flag, then the data.

#[derive(Clone, Copy, PartialEq)]
enum Publish {
    Relaxed,
    ReleaseAcquire,
    Fences,
    /// A Release store, then a Relaxed load and an Acquire fence.
    ReleaseThenAcquireFence,
}

fn message_passing(publish: Publish) -> (bool, usize) {
    let data = Arc::new(AtomicUsize::new(0));
    let flag = Arc::new(AtomicBool::new(false));
    let (store, load) = match publish {
        Publish::Relaxed | Publish::Fences => (Relaxed, Relaxed),
        Publish::ReleaseAcquire => (Release, Acquire),
        Publish::ReleaseThenAcquireFence => (Release, Relaxed),
    };
    let writer = {
        let (data, flag) = (Arc::clone(&data), Arc::clone(&flag));
        thread::spawn(move || {
            data.store(42, Relaxed);
            if publish == Publish::Fences {
                fence(Release);
            }
            flag.store(true, store);
        })
    };
    let seen_flag = flag.load(load);
    if matches!(publish, Publish::Fences | Publish::ReleaseThenAcquireFence) {
        fence(Acquire);
    }
    let seen_data = data.load(Relaxed);
    writer.join().unwrap();
    (seen_flag, seen_data)
}

#[test]
fn message_passing_relaxed_can_see_the_flag_without_the_data() {
    let seen = all_outcomes(|| message_passing(Publish::Relaxed));
    assert!(seen.contains(&(true, 0)), "{seen:?}");
}

#[test]
fn message_passing_release_acquire_always_sees_the_data() {
    let seen = all_outcomes(|| message_passing(Publish::ReleaseAcquire));
    assert_eq!(seen, HashSet::from([(false, 0), (false, 42), (true, 42)]));
}

#[test]
fn message_passing_with_fences_always_sees_the_data() {
    let seen = all_outcomes(|| message_passing(Publish::Fences));
    assert!(!seen.contains(&(true, 0)), "{seen:?}");
    assert!(seen.contains(&(true, 42)), "{seen:?}");
}

#[test]
fn message_passing_with_an_acquire_fence_always_sees_the_data() {
    let seen = all_outcomes(|| message_passing(Publish::ReleaseThenAcquireFence));
    assert!(!seen.contains(&(true, 0)), "{seen:?}");
    assert!(seen.contains(&(true, 42)), "{seen:?}");
}

// Coherence: the loads of one location agree with its modification order.

#[test]
fn loads_never_go_back_in_modification_order() {
    let seen = all_outcomes(|| {
        let x = Arc::new(AtomicUsize::new(0));
        let writer = {
            let x = Arc::clone(&x);
            thread::spawn(move || {
                x.store(1, Relaxed);
                x.store(2, Relaxed);
            })
        };
        let first = x.load(Relaxed);
        let second = x.load(Relaxed);
        writer.join().unwrap();
        (first, second)
    });
    assert!(seen.iter().all(|(first, second)| first <= second), "{seen:?}");
    // Some of these need a load to read an older store after the writer has run.
    assert!(seen.len() >= 5, "{seen:?}");
}

#[test]
fn a_load_never_reads_older_than_its_own_tasks_store() {
    let seen = all_outcomes(|| {
        let x = Arc::new(AtomicUsize::new(0));
        let other = {
            let x = Arc::clone(&x);
            thread::spawn(move || x.store(2, Relaxed))
        };
        x.store(1, Relaxed);
        let seen = x.load(Relaxed);
        other.join().unwrap();
        seen
    });
    assert_eq!(seen, HashSet::from([1, 2]));
}

#[test]
fn a_join_makes_every_store_visible() {
    // PCT refuses a test whose threads never run at the same time, so this uses only the random
    // scheduler.
    let seen = outcomes(RandomScheduler::new_from_seed(SEED, ITERATIONS), || {
        let x = Arc::new(AtomicUsize::new(0));
        let writer = {
            let x = Arc::clone(&x);
            thread::spawn(move || {
                x.store(1, Relaxed);
                x.store(2, Relaxed);
            })
        };
        writer.join().unwrap();
        x.load(Relaxed)
    });
    assert_eq!(seen, HashSet::from([2]));
}

// Read-modify-writes read the newest store.

#[test]
fn read_modify_writes_never_lose_an_update() {
    let seen = all_outcomes(|| {
        let x = Arc::new(AtomicUsize::new(0));
        let threads = (0..3)
            .map(|_| {
                let x = Arc::clone(&x);
                thread::spawn(move || x.fetch_add(1, Relaxed))
            })
            .collect::<Vec<_>>();
        let mut old = threads.into_iter().map(|t| t.join().unwrap()).collect::<Vec<_>>();
        old.sort();
        (old, x.load(Relaxed))
    });
    assert_eq!(seen, HashSet::from([(vec![0, 1, 2], 3)]));
}

#[test]
fn a_swap_stays_next_to_the_store_it_read() {
    // Loom 0.7 produces the forbidden outcome here (tokio-rs/loom#254).
    let seen = all_outcomes(|| {
        let flag = Arc::new(AtomicUsize::new(0));
        let data = Arc::new(AtomicUsize::new(0));
        let reader = {
            let (flag, data) = (Arc::clone(&flag), Arc::clone(&data));
            thread::spawn(move || {
                flag.store(1, Relaxed);
                fence(SeqCst);
                data.load(Relaxed)
            })
        };
        let swapped = flag.swap(2, Relaxed);
        data.store(1, Relaxed);
        fence(SeqCst);
        let flag_after = flag.load(Relaxed);
        (swapped, reader.join().unwrap(), flag_after)
    });
    assert!(!seen.contains(&(0, 0, 2)), "{seen:?}");
}

#[test]
fn a_failed_compare_exchange_reads_the_newest_store() {
    // As above, the threads never run at the same time.
    let seen = outcomes(RandomScheduler::new_from_seed(SEED, ITERATIONS), || {
        let x = Arc::new(AtomicUsize::new(0));
        let writer = {
            let x = Arc::clone(&x);
            thread::spawn(move || x.store(1, Relaxed))
        };
        writer.join().unwrap();
        x.compare_exchange(5, 6, Relaxed, Relaxed)
    });
    assert_eq!(seen, HashSet::from([Err(1)]));
}

// Release sequences: a read-modify-write continues the release sequence of the store it read, and a
// plain store ends it.

fn release_sequence(read_modify_write: bool) -> (usize, usize) {
    let data = Arc::new(AtomicUsize::new(0));
    let flag = Arc::new(AtomicUsize::new(0));
    let writer = {
        let (data, flag) = (Arc::clone(&data), Arc::clone(&flag));
        thread::spawn(move || {
            data.store(42, Relaxed);
            flag.store(1, Release);
        })
    };
    let bumper = {
        let flag = Arc::clone(&flag);
        thread::spawn(move || {
            if read_modify_write {
                let _ = flag.compare_exchange(1, 2, Relaxed, Relaxed);
            } else if flag.load(Relaxed) == 1 {
                flag.store(2, Relaxed);
            }
        })
    };
    let seen_flag = flag.load(Acquire);
    let seen_data = data.load(Relaxed);
    writer.join().unwrap();
    bumper.join().unwrap();
    (seen_flag, seen_data)
}

#[test]
fn a_read_modify_write_continues_a_release_sequence() {
    let seen = all_outcomes(|| release_sequence(true));
    assert!(!seen.contains(&(2, 0)), "{seen:?}");
    assert!(seen.contains(&(2, 42)), "{seen:?}");
}

#[test]
fn a_relaxed_store_ends_a_release_sequence() {
    let seen = all_outcomes(|| release_sequence(false));
    assert!(seen.contains(&(2, 0)), "{seen:?}");
}

// Independent reads of independent writes.

fn iriw(store: Ordering, load: Ordering) -> (usize, usize, usize, usize) {
    let x = Arc::new(AtomicUsize::new(0));
    let y = Arc::new(AtomicUsize::new(0));
    let writers = [Arc::clone(&x), Arc::clone(&y)].map(|location| thread::spawn(move || location.store(1, store)));
    let x_then_y = {
        let (x, y) = (Arc::clone(&x), Arc::clone(&y));
        thread::spawn(move || (x.load(load), y.load(load)))
    };
    let y_then_x = {
        let (x, y) = (Arc::clone(&x), Arc::clone(&y));
        thread::spawn(move || (y.load(load), x.load(load)))
    };
    for writer in writers {
        writer.join().unwrap();
    }
    let (a, b) = x_then_y.join().unwrap();
    let (c, d) = y_then_x.join().unwrap();
    (a, b, c, d)
}

#[test]
fn iriw_seqcst_readers_agree_on_the_order_of_the_writes() {
    let seen = all_outcomes(|| iriw(SeqCst, SeqCst));
    assert!(!seen.contains(&(1, 0, 1, 0)), "{seen:?}");
}

#[test]
fn iriw_release_acquire_readers_can_disagree() {
    let seen = all_outcomes(|| iriw(Release, Acquire));
    assert!(seen.contains(&(1, 0, 1, 0)), "{seen:?}");
}

// What the emulation does with Shuttle's vector clocks.

#[test]
fn a_relaxed_load_acquires_nothing_until_an_acquire_fence() {
    let mut config = weak_memory_config();
    config.max_steps = shuttle::MaxSteps::FailAfter(10_000);
    Runner::new(RandomScheduler::new_from_seed(SEED, 200), config).run(|| {
        let flag = Arc::new(AtomicBool::new(false));
        let writer = {
            let flag = Arc::clone(&flag);
            thread::spawn(move || flag.store(true, Release))
        };
        while !flag.load(Relaxed) {
            thread::yield_now();
        }
        // Only the main thread's own entry is set: the spawned thread's store doesn't happen before
        // a Relaxed load of it.
        check_clock(|i, c| (c > 0) == (i == 0));
        fence(Acquire);
        check_clock(|i, c| (c > 0) == (i == 0 || i == 1));
        writer.join().unwrap();
    });
}

#[test]
fn an_acquire_load_acquires_the_store_it_reads() {
    let mut config = weak_memory_config();
    config.max_steps = shuttle::MaxSteps::FailAfter(10_000);
    Runner::new(RandomScheduler::new_from_seed(SEED, 200), config).run(|| {
        let flag = Arc::new(AtomicBool::new(false));
        let writer = {
            let flag = Arc::clone(&flag);
            thread::spawn(move || flag.store(true, Release))
        };
        let seen = flag.load(Acquire);
        check_clock(|i, c| (c > 0) == (i == 0 || (i == 1 && seen)));
        writer.join().unwrap();
    });
}

#[test]
fn seqcst_accesses_synchronize_like_release_and_acquire() {
    // With only SeqCst accesses the clocks are what they are without the emulation: a reader
    // inherits the clock of the writer it read from, and nothing else.
    let seen = all_outcomes(|| {
        let x = Arc::new(AtomicUsize::new(0));
        let y = Arc::new(AtomicUsize::new(0));
        let writer = {
            let (x, y) = (Arc::clone(&x), Arc::clone(&y));
            thread::spawn(move || {
                x.store(1, SeqCst);
                y.store(1, SeqCst);
            })
        };
        let reader = {
            let (x, y) = (Arc::clone(&x), Arc::clone(&y));
            thread::spawn(move || {
                let y = y.load(SeqCst);
                let x = x.load(SeqCst);
                check_clock(|i, c| (c > 0) == (i == 0 || i == 2 || (i == 1 && x + y > 0)));
                (x, y)
            })
        };
        writer.join().unwrap();
        reader.join().unwrap()
    });
    assert_eq!(seen, HashSet::from([(0, 0), (1, 0), (1, 1)]));
}

// Ported from Miri's `tests/pass/0weak_memory/weak.rs`. Miri runs these with preemption off, so its
// threads run in the order they were spawned. Here a thread waits on `Progress`, a std atomic that
// Shuttle doesn't model, so it runs after the others without synchronizing with them.

#[derive(Clone, Default)]
struct Progress(Arc<std::sync::atomic::AtomicUsize>);

impl Progress {
    fn done(&self) {
        self.0.fetch_add(1, SeqCst);
    }

    /// Yields until `done` has been called `n` times.
    fn wait_for(&self, n: usize) {
        while self.0.load(SeqCst) < n {
            thread::yield_now();
        }
    }
}

#[test]
fn a_seqcst_load_never_reads_an_older_seqcst_store() {
    // Figure 8 of the paper Miri's emulation follows: the SeqCst load can read the Relaxed store even
    // after both other threads have run, but never the first SeqCst store.
    let seen = all_outcomes(|| {
        let x = Arc::new(AtomicUsize::new(0));
        let progress = Progress::default();
        let relaxed = {
            let (x, progress) = (Arc::clone(&x), progress.clone());
            thread::spawn(move || {
                x.store(1, Relaxed);
                progress.done();
            })
        };
        let seqcst = {
            let (x, progress) = (Arc::clone(&x), progress.clone());
            thread::spawn(move || {
                x.store(2, SeqCst);
                x.store(3, SeqCst);
                progress.done();
            })
        };
        let reader = {
            let (x, progress) = (Arc::clone(&x), progress.clone());
            thread::spawn(move || {
                progress.wait_for(2);
                x.load(SeqCst)
            })
        };
        relaxed.join().unwrap();
        seqcst.join().unwrap();
        reader.join().unwrap()
    });
    assert_eq!(seen, HashSet::from([1, 3]));
}

#[test]
fn an_old_release_store_still_releases() {
    // A load that reads an older Release store acquires what that store released.
    let seen = all_outcomes(|| {
        let x = Arc::new(AtomicUsize::new(0));
        let y = Arc::new(AtomicUsize::new(0));
        let progress = Progress::default();
        let writer = {
            let (x, y, progress) = (Arc::clone(&x), Arc::clone(&y), progress.clone());
            thread::spawn(move || {
                y.store(1, Relaxed);
                x.store(1, Release);
                x.store(3, Relaxed);
                progress.done();
            })
        };
        let reader = {
            let (x, y, progress) = (Arc::clone(&x), Arc::clone(&y), progress.clone());
            thread::spawn(move || {
                progress.wait_for(1);
                (x.load(Acquire) == 1).then(|| y.load(Relaxed))
            })
        };
        writer.join().unwrap();
        reader.join().unwrap()
    });
    assert_eq!(seen, HashSet::from([None, Some(1)]));
}

#[test]
fn a_relaxed_store_by_the_same_thread_ends_a_release_sequence() {
    // C++20 made this so (P0982R1): reading the Relaxed store no longer acquires the Release store
    // before it.
    let seen = all_outcomes(|| {
        let x = Arc::new(AtomicUsize::new(0));
        let y = Arc::new(AtomicUsize::new(0));
        let progress = Progress::default();
        let other = {
            let (x, progress) = (Arc::clone(&x), progress.clone());
            thread::spawn(move || {
                x.store(2, Relaxed);
                progress.done();
            })
        };
        let writer = {
            let (x, y, progress) = (Arc::clone(&x), Arc::clone(&y), progress.clone());
            thread::spawn(move || {
                y.store(1, Relaxed);
                x.store(1, Release);
                x.store(3, Relaxed);
                progress.done();
            })
        };
        let reader = {
            let (x, y, progress) = (Arc::clone(&x), Arc::clone(&y), progress.clone());
            thread::spawn(move || {
                progress.wait_for(2);
                (x.load(Acquire) == 3).then(|| y.load(Relaxed))
            })
        };
        other.join().unwrap();
        writer.join().unwrap();
        reader.join().unwrap()
    });
    assert_eq!(seen, HashSet::from([None, Some(0), Some(1)]));
}

#[test]
fn fences_around_a_load_that_reads_no_store_do_not_synchronize() {
    // From https://github.com/llvm/llvm-project/issues/56450#issuecomment-1183695905: replacing a
    // `fetch_add(0)` with a load between two fences lets both threads read 0.
    fn store_fence_load(storing: &AtomicUsize, sync: &AtomicUsize, loading: &AtomicUsize) -> usize {
        storing.store(1, Relaxed);
        fence(Release);
        sync.load(Relaxed);
        fence(Acquire);
        loading.load(Relaxed)
    }

    let seen = all_outcomes(|| {
        let x = Arc::new(AtomicUsize::new(0));
        let y = Arc::new(AtomicUsize::new(0));
        let z = Arc::new(AtomicUsize::new(0));
        let first = {
            let (x, y, z) = (Arc::clone(&x), Arc::clone(&y), Arc::clone(&z));
            thread::spawn(move || store_fence_load(&y, &x, &z))
        };
        let second = {
            let (x, y, z) = (Arc::clone(&x), Arc::clone(&y), Arc::clone(&z));
            thread::spawn(move || store_fence_load(&z, &x, &y))
        };
        (first.join().unwrap(), second.join().unwrap())
    });
    assert!(seen.contains(&(0, 0)), "{seen:?}");
}

#[test]
fn a_load_after_a_seqcst_fence_never_reads_before_an_earlier_seqcst_store() {
    // C++17 §32.4 [atomics.order] paragraph 4. The SeqCst store doesn't synchronize with anything,
    // but it comes before the fence in the single total order of SeqCst operations.
    let seen = all_outcomes(|| {
        let x = Arc::new(AtomicUsize::new(0));
        let progress = Progress::default();
        let writer = {
            let (x, progress) = (Arc::clone(&x), progress.clone());
            thread::spawn(move || {
                x.store(1, SeqCst);
                progress.done();
            })
        };
        let reader = {
            let (x, progress) = (Arc::clone(&x), progress.clone());
            thread::spawn(move || {
                progress.wait_for(1);
                fence(SeqCst);
                x.load(Relaxed)
            })
        };
        writer.join().unwrap();
        reader.join().unwrap()
    });
    assert_eq!(seen, HashSet::from([1]));
}

#[test]
fn a_seqcst_load_never_reads_before_a_store_that_precedes_an_earlier_seqcst_fence() {
    // C++17 §32.4 [atomics.order] paragraph 5. The fence doesn't synchronize with the load, but it
    // comes before it in the single total order of SeqCst operations.
    let seen = all_outcomes(|| {
        let x = Arc::new(AtomicUsize::new(0));
        let progress = Progress::default();
        let writer = {
            let (x, progress) = (Arc::clone(&x), progress.clone());
            thread::spawn(move || {
                x.store(1, Relaxed);
                fence(SeqCst);
                progress.done();
            })
        };
        let reader = {
            let (x, progress) = (Arc::clone(&x), progress.clone());
            thread::spawn(move || {
                progress.wait_for(1);
                x.load(SeqCst)
            })
        };
        writer.join().unwrap();
        reader.join().unwrap()
    });
    assert_eq!(seen, HashSet::from([1]));
}

// Static atomics outlive an execution.

static COUNTER: AtomicUsize = AtomicUsize::new(0);

#[test]
fn a_static_atomic_starts_each_execution_from_its_value() {
    // Without starting over, the second execution's load could read the first execution's stores,
    // which neither happen before it nor were read by a load that does.
    let last = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let last_by_test = Arc::clone(&last);
    Runner::new(RandomScheduler::new_from_seed(SEED, 500), weak_memory_config()).run(move || {
        assert_eq!(COUNTER.load(Relaxed), last_by_test.load(SeqCst));
        let other = thread::spawn(|| {
            COUNTER.fetch_add(1, Relaxed);
            COUNTER.fetch_add(1, AcqRel);
        });
        other.join().unwrap();
        last_by_test.store(COUNTER.load(Relaxed), SeqCst);
    });
    assert_eq!(last.load(SeqCst), 1_000);
}

// Schedulers.

/// Records the schedule of its latest execution, which is the one the runner prints when a test
/// fails.
struct Recording<S> {
    inner: S,
    schedule: Arc<Mutex<Schedule>>,
}

impl<S: Scheduler> Scheduler for Recording<S> {
    fn new_execution(&mut self) -> Option<Schedule> {
        let schedule = self.inner.new_execution()?;
        *self.schedule.lock().unwrap() = schedule.clone();
        Some(schedule)
    }

    fn next_task(
        &mut self,
        runnable_tasks: &[&Task],
        current_task: Option<TaskId>,
        is_yielding: bool,
    ) -> Option<TaskId> {
        let next = self.inner.next_task(runnable_tasks, current_task, is_yielding)?;
        self.schedule.lock().unwrap().push_task(next);
        Some(next)
    }

    fn next_u64(&mut self) -> u64 {
        self.schedule.lock().unwrap().push_random();
        self.inner.next_u64()
    }
}

#[test]
fn a_failing_schedule_replays() {
    fn test() {
        let (seen_flag, seen_data) = message_passing(Publish::Relaxed);
        assert!(!seen_flag || seen_data == 42, "saw the flag without the data");
    }

    fn failure(scheduler: impl Scheduler + 'static) -> String {
        let payload = panic::catch_unwind(panic::AssertUnwindSafe(move || {
            let mut config = weak_memory_config();
            config.failure_persistence = FailurePersistence::None;
            Runner::new(scheduler, config).run(test);
        }))
        .expect_err("the test should fail");
        match payload.downcast::<String>() {
            Ok(message) => *message,
            Err(payload) => payload.downcast::<&str>().unwrap().to_string(),
        }
    }

    let schedule = Arc::new(Mutex::new(Schedule::new(0)));
    let message = failure(Recording {
        inner: RandomScheduler::new_from_seed(SEED, ITERATIONS),
        schedule: Arc::clone(&schedule),
    });
    assert_eq!(message, "saw the flag without the data");
    let schedule = schedule.lock().unwrap().clone();
    // The failure needs a load to read an older store, which the schedule records as a random step.
    assert!(format!("{schedule:?}").contains("Random"), "{schedule:?}");
    assert_eq!(failure(ReplayScheduler::new_from_schedule(schedule)), message);
}

#[test]
#[should_panic(expected = "Config::weak_memory")]
fn dfs_without_random_data_panics_when_a_load_has_a_choice() {
    Runner::new(DfsScheduler::new(None, false), weak_memory_config()).run(|| {
        message_passing(Publish::Relaxed);
    });
}

#[test]
fn dfs_with_random_data_runs() {
    Runner::new(DfsScheduler::new(None, true), weak_memory_config()).run(|| {
        message_passing(Publish::ReleaseAcquire);
    });
}

#[test]
fn without_the_config_atomics_are_seqcst() {
    let seen = Arc::new(Mutex::new(HashSet::new()));
    let seen_by_test = Arc::clone(&seen);
    let mut config = Config::new();
    config.silence_warnings = true;
    Runner::new(RandomScheduler::new_from_seed(SEED, ITERATIONS), config).run(move || {
        seen_by_test
            .lock()
            .unwrap()
            .insert(store_buffering(Relaxed, Relaxed, false));
    });
    assert!(!seen.lock().unwrap().contains(&(0, 0)));
}
