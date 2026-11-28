//! The free functions and `Rng::new` under Shuttle: where their values come from, and what that means for replay.

use shuttle::sync::{Arc, Mutex};
use shuttle::{check_dfs, check_random, check_random_with_seed, check_uncontrolled_nondeterminism, current, thread};
use shuttle_fastrand_inner as fastrand;
use shuttle_fastrand_inner::Rng;
use std::collections::HashSet;

/// A name, and a function that draws a value through that entry point.
type EntryPoint = (&'static str, fn() -> u64);

/// Every way fastrand has of reaching its thread-local generator, as a function that draws a `u64` with it.
const ENTRY_POINTS: &[EntryPoint] = &[
    ("u64", || fastrand::u64(..)),
    ("usize", || fastrand::usize(..) as u64),
    ("i128", || fastrand::i128(..) as u64),
    ("u32", || u64::from(fastrand::u32(..))),
    ("f64", || fastrand::f64().to_bits()),
    ("fill", || {
        let mut bytes = [0u8; 8];
        fastrand::fill(&mut bytes);
        u64::from_le_bytes(bytes)
    }),
    ("shuffle", || {
        let mut deck: Vec<u64> = (0..20).collect();
        fastrand::shuffle(&mut deck);
        deck.iter()
            .fold(0, |acc, card| acc.wrapping_mul(31).wrapping_add(*card))
    }),
    ("choose_multiple", || {
        let picked = fastrand::choose_multiple(0..1000u64, 10);
        picked
            .iter()
            .fold(0, |acc, value| acc.wrapping_mul(1009).wrapping_add(*value))
    }),
    ("get_seed", fastrand::get_seed),
    ("Rng::new", || Rng::new().u64(..)),
    ("Rng::default", || Rng::default().u64(..)),
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
        // The values differ between threads and between executions.
        let distinct = first.iter().flatten().collect::<HashSet<_>>();
        assert_eq!(distinct.len(), 3 * 50, "{name}");
    }
}

#[test]
fn each_task_has_its_own_generator() {
    check_random(
        || {
            let threads = (0..3u64)
                .map(|seed| {
                    thread::spawn(move || {
                        fastrand::seed(seed);
                        let mut expected = Rng::with_seed(seed);
                        for _ in 0..5 {
                            // Let the other threads draw in between: they draw from generators of their own.
                            thread::yield_now();
                            assert_eq!(fastrand::u64(..), expected.u64(..));
                        }
                    })
                })
                .collect::<Vec<_>>();
            for thread in threads {
                thread.join().unwrap();
            }
        },
        50,
    );
}

#[test]
fn each_execution_starts_without_a_generator() {
    check_random(
        || {
            // If the generator carried over from the previous execution, it would hold the seed set below.
            assert_ne!(fastrand::get_seed(), 5);
            fastrand::seed(5);
        },
        50,
    );
}

#[test]
#[should_panic(expected = "fastrand's own generator carried over")]
fn fastrands_own_generator_carries_over_between_executions() {
    check_random(
        || {
            assert_ne!(
                fastrand_orig::get_seed(),
                5,
                "fastrand's own generator carried over from the previous execution"
            );
            fastrand_orig::seed(5);
        },
        50,
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
fn the_same_schedule_with_fastrands_own_functions_does_not_replay() {
    check_uncontrolled_nondeterminism(|| lock_if_last_bit_set(|| fastrand_orig::u64(..)), 20);
}

#[test]
fn drawing_is_not_a_scheduling_point() {
    check_random(
        || {
            let before = current::context_switches();
            for (_, draw) in ENTRY_POINTS {
                draw();
            }
            fastrand::seed(1);
            assert_eq!(current::context_switches(), before);
        },
        5,
    );
}

#[test]
#[should_panic(expected = "requested random data from DFS scheduler")]
fn check_dfs_refuses_an_unseeded_generator() {
    check_dfs(
        || {
            let _ = fastrand::u64(..);
        },
        None,
    );
}

#[test]
fn check_dfs_accepts_a_seeded_one() {
    check_dfs(
        || {
            fastrand::seed(1);
            let _ = fastrand::u64(..);
            let _ = Rng::new().u64(..);
            let _ = Rng::with_seed(2).alphanumeric();
        },
        None,
    );
}

#[test]
fn the_free_functions_panic_outside_of_shuttle() {
    for (name, draw) in ENTRY_POINTS {
        let result = std::panic::catch_unwind(*draw);
        assert!(result.is_err(), "{name} should panic outside of a Shuttle test");
    }
    assert!(std::panic::catch_unwind(|| fastrand::seed(1)).is_err());
    // A seeded generator works anywhere.
    assert_eq!(Rng::with_seed(3).u64(..), fastrand_orig::Rng::with_seed(3).u64(..));
}

#[test]
fn a_panicking_draw_leaves_the_generator_as_fastrands_does() {
    // fastrand's, outside of Shuttle: the panic on the empty range leaves the seed in place.
    fastrand_orig::seed(3);
    assert!(std::panic::catch_unwind(|| fastrand_orig::u8(5..5)).is_err());
    assert_eq!(fastrand_orig::get_seed(), 3);

    check_random(
        || {
            fastrand::seed(3);
            assert!(std::panic::catch_unwind(|| fastrand::u8(5..5)).is_err());
            assert_eq!(fastrand::get_seed(), 3);
        },
        1,
    );
}
