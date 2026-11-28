//! The entry points under Shuttle: where their values come from, and what that means for replay.

use shuttle::sync::{Arc, Mutex};
use shuttle::{check_dfs, check_random, check_random_with_seed, check_uncontrolled_nondeterminism, current, thread};
use shuttle_rand_0_10_inner as rand;
use shuttle_rand_0_10_inner::rngs::{ChaCha8Rng, SmallRng, StdRng, SysRng, ThreadRng};
use shuttle_rand_0_10_inner::{Rng, RngExt, SeedableRng, TryRng};
use std::collections::HashSet;

/// A name, and a function that draws a value through that entry point.
type EntryPoint = (&'static str, fn() -> u64);

/// Every entry point that reaches entropy in rand 0.10, as a function that draws a `u64` with it.
const ENTRY_POINTS: &[EntryPoint] = &[
    ("random", || rand::random()),
    ("rng", || rand::rng().next_u64()),
    ("ThreadRng::default", || ThreadRng::default().next_u64()),
    ("random_iter", || rand::random_iter().next().unwrap()),
    ("random_range", || rand::random_range(0..=u64::MAX)),
    ("random_bool", || u64::from(rand::random_bool(0.5))),
    ("random_ratio", || u64::from(rand::random_ratio(1, 2))),
    ("fill", || {
        let mut bytes = [0u8; 8];
        rand::fill(&mut bytes);
        u64::from_le_bytes(bytes)
    }),
    ("make_rng", || rand::make_rng::<StdRng>().next_u64()),
    ("SysRng", || SysRng.try_next_u64().unwrap()),
    ("try_from_rng(SysRng)", || {
        SmallRng::try_from_rng(&mut SysRng).unwrap().random()
    }),
];

/// Spawns `num_threads` threads that each draw a value with `draw`, and returns the values in the order the
/// threads drew them, which depends on the schedule.
fn values_from_threads(num_threads: usize, draw: fn() -> u64) -> Vec<u64> {
    let values = Arc::new(Mutex::new(Vec::new()));
    let threads = (0..num_threads)
        .map(|_| {
            let values = values.clone();
            thread::spawn(move || {
                let value = draw();
                values.lock().unwrap().push(value);
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    values.lock().unwrap().clone()
}

/// Runs `check_random_with_seed` and returns the values that each execution drew.
fn values_with_seed(seed: u64, iterations: usize, draw: fn() -> u64) -> Vec<Vec<u64>> {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log_clone = log.clone();
    check_random_with_seed(
        move || {
            let values = values_from_threads(3, draw);
            log_clone.lock().unwrap().push(values);
        },
        seed,
        iterations,
    );
    log.lock().unwrap().clone()
}

#[test]
fn same_seed_gives_the_same_values() {
    for (name, draw) in ENTRY_POINTS {
        let first = values_with_seed(0x5eed, 50, *draw);
        assert_eq!(values_with_seed(0x5eed, 50, *draw), first, "{name}");
        assert_ne!(values_with_seed(0x5eed + 1, 50, *draw), first, "{name}");
        if !matches!(*name, "random_bool" | "random_ratio") {
            // The values differ between threads and between executions.
            let distinct = first.iter().flatten().collect::<HashSet<_>>();
            assert_eq!(distinct.len(), 3 * 50, "{name}");
        }
    }
}

#[test]
fn seeded_generators_are_rands() {
    check_random(
        || {
            let mut ours = StdRng::seed_from_u64(42);
            let mut rands = <rand_orig::rngs::StdRng as rand_orig::SeedableRng>::seed_from_u64(42);
            assert_eq!(ours.random::<[u64; 4]>(), rands.random::<[u64; 4]>());
            let mut ours = ChaCha8Rng::from_seed([9; 32]);
            let mut rands = <rand_orig::rngs::ChaCha8Rng as rand_orig::SeedableRng>::from_seed([9; 32]);
            assert_eq!(ours.next_u64(), rands.next_u64());
        },
        5,
    );
}

/// Each of 10 threads draws a value with `draw`, and takes the lock only if its last bit is set, so the values
/// decide which threads contend for the lock and how the schedule goes.
fn lock_if_last_bit_set(draw: fn() -> u64) {
    let lock = Arc::new(Mutex::new(0u64));
    let threads = (0..10)
        .map(|_| {
            let lock = lock.clone();
            thread::spawn(move || {
                if draw() & 1 == 1 {
                    *lock.lock().unwrap() += 1;
                }
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn a_schedule_that_depends_on_the_values_replays() {
    for (_, draw) in ENTRY_POINTS {
        check_uncontrolled_nondeterminism(move || lock_if_last_bit_set(*draw), 20);
    }
}

#[test]
#[should_panic(expected = "possible nondeterminism")]
fn the_same_schedule_with_rands_own_entry_points_does_not_replay() {
    check_uncontrolled_nondeterminism(|| lock_if_last_bit_set(rand_orig::random), 20);
}

#[test]
fn drawing_is_not_a_scheduling_point() {
    check_random(
        || {
            let before = current::context_switches();
            for (_, draw) in ENTRY_POINTS {
                draw();
            }
            assert_eq!(current::context_switches(), before);
        },
        5,
    );
}

#[test]
#[should_panic(expected = "requested random data from DFS scheduler")]
fn check_dfs_refuses_the_entry_points() {
    check_dfs(
        || {
            let _: u64 = rand::random();
        },
        None,
    );
}

#[test]
fn check_dfs_accepts_seeded_generators() {
    check_dfs(
        || {
            let mut seeded = StdRng::seed_from_u64(1);
            let _: u64 = seeded.random();
            let _ = SmallRng::from_rng(&mut seeded);
        },
        None,
    );
}

#[test]
fn the_entry_points_panic_outside_of_shuttle() {
    for (name, draw) in ENTRY_POINTS {
        let result = std::panic::catch_unwind(*draw);
        assert!(result.is_err(), "{name} should panic outside of a Shuttle test");
    }
    // The seeded generators work anywhere.
    assert_eq!(
        StdRng::seed_from_u64(3).next_u64(),
        <rand_orig::rngs::StdRng as rand_orig::SeedableRng>::seed_from_u64(3).next_u64()
    );
}

#[test]
fn reseed_does_nothing() {
    check_random(
        || {
            let mut rng = rand::rng();
            assert!(rng.reseed().is_ok());
            let _: u64 = rng.random();
        },
        1,
    );
}
