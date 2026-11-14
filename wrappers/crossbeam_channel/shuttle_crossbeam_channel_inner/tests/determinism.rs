//! Replay: a selection's choice comes from Shuttle's random number generator, so the same seed
//! gives the same choices, and `check_uncontrolled_nondeterminism` accepts them. crossbeam's own
//! selection draws from a generator Shuttle does not control.

mod common;

use common::per_execution;
use shuttle::{check_uncontrolled_nondeterminism, thread};
use shuttle_crossbeam_channel_inner::{after, bounded, select, tick, unbounded};
use shuttle_time_inner::time::Duration;

/// Eight selections between two ready arms, and which arm each chose.
fn choices() -> Vec<u8> {
    let (s1, r1) = unbounded();
    let (s2, r2) = unbounded();
    (0..8)
        .map(|_| {
            s1.send(1).unwrap();
            s2.send(2).unwrap();
            let picked = select! {
                recv(r1) -> msg => msg.unwrap(),
                recv(r2) -> msg => msg.unwrap(),
            };
            // Empty the other channel for the next round.
            let _ = r1.try_recv();
            let _ = r2.try_recv();
            picked
        })
        .collect()
}

#[test]
fn the_same_seed_gives_the_same_choices() {
    assert_eq!(per_execution(choices, 7, 20), per_execution(choices, 7, 20));
}

#[test]
fn a_different_seed_gives_different_choices() {
    assert_ne!(per_execution(choices, 7, 20), per_execution(choices, 8, 20));
}

#[test]
fn the_executions_of_one_run_differ() {
    let runs = per_execution(choices, 9, 20);
    assert!(runs.iter().any(|run| *run != runs[0]));
}

/// Spawns a thread or not depending on which arm a selection picks, so that a choice Shuttle does
/// not control changes the schedule.
fn branch_on_a_selection() {
    let (s1, r1) = unbounded();
    let (s2, r2) = unbounded();
    s1.send(()).unwrap();
    s2.send(()).unwrap();
    let first = select! {
        recv(r1) -> _ => true,
        recv(r2) -> _ => false,
    };
    if first {
        thread::spawn(|| {}).join().unwrap();
    }
}

#[test]
fn a_selection_is_controlled_nondeterminism() {
    check_uncontrolled_nondeterminism(branch_on_a_selection, 100);
}

#[test]
#[should_panic(expected = "possible nondeterminism")]
fn crossbeams_own_selection_is_not() {
    check_uncontrolled_nondeterminism(
        || {
            let (s1, r1) = crossbeam_channel::unbounded();
            let (s2, r2) = crossbeam_channel::unbounded();
            s1.send(()).unwrap();
            s2.send(()).unwrap();
            let first = crossbeam_channel::select! {
                recv(r1) -> _ => true,
                recv(r2) -> _ => false,
            };
            if first {
                thread::spawn(|| {}).join().unwrap();
            }
        },
        100,
    );
}

// Blocking, waking, timeouts and timers replay too.
#[test]
fn waits_and_timers_replay() {
    check_uncontrolled_nondeterminism(
        || {
            let (s, r) = bounded::<u32>(1);
            let ticker = tick(Duration::from_millis(3));
            let producer = thread::spawn(move || {
                for n in 0..3 {
                    ticker.recv().unwrap();
                    s.send_timeout(n, Duration::from_millis(5)).ok();
                }
            });
            let mut got = Vec::new();
            loop {
                select! {
                    recv(r) -> msg => match msg {
                        Ok(n) => got.push(n),
                        Err(_) => break,
                    },
                    recv(after(Duration::from_millis(4))) -> _ => got.push(99),
                }
            }
            producer.join().unwrap();
            if got.contains(&99) {
                thread::spawn(|| {}).join().unwrap();
            }
        },
        100,
    );
}
