//! Transition parity: after the main task upgrades or downgrades its lock, does Shuttle's `RwLock`
//! behave like `parking_lot`?
//!
//! Each scenario has three tasks on one lock:
//!
//! * The main task runs one of the harness's `TRANSITIONS` programs: it takes the lock, spawns the
//!   other two tasks, and then upgrades or downgrades its lock.
//! * The second task makes the `queued` requests, then unlocks.
//! * The third task makes the `requested` requests, then unlocks.
//!
//! Each scenario has two variants:
//!
//! * `release`: the main task unlocks after its transitions. This variant checks the values that the
//!   tasks record (see the reference module docs): for example, a writer that gets in while the main
//!   task upgrades changes the value that the main task sees.
//! * `hold`: the main task keeps the lock after its transitions until the third task sends a signal.
//!   This is the admission check of `rwlock_admission_parity`, applied to the lock mode after the
//!   transitions.
//!
//! [`KNOWN_DIVERGENCES`] lists the scenarios where the Shuttle model is known to be wrong. A new
//! divergence fails the test. So does a known divergence that no longer happens: remove it from the
//! list when the model is fixed. Run with `--nocapture` to see the full table.

// Each test crate uses only part of the shared module.
#[allow(dead_code)]
mod rwlock_parity;

use rwlock_parity::harness::Kind::*;
use rwlock_parity::harness::{KnownDivergence, QUEUED, REQUESTED, Scenario, TRANSITIONS, check_table, describe};
use rwlock_parity::reference::Op::{self, *};

/// Each entry is explained by the same cause as the known divergences of `rwlock_admission_parity`:
/// Shuttle's strictly fair `BatchSemaphore` refuses a plain read while a waiter is queued, and
/// `parking_lot` refuses a plain read only while `WRITER_BIT` is set.
///
/// * `downgrade_to_upgradable`, `hold`: after the downgrade the main task holds an upgradable read.
///   A queued `write` or `upgradable_read` is at the head of Shuttle's queue, and no request can pass
///   it (`BatchSemaphoreState` invariant 1), so the plain read waits for ever. `parking_lot` admits
///   the read at once.
/// * `downgrade_upgradable` with a queued `upgradable_read`: before the downgrade, the main task
///   holds an upgradable read and the second task waits for one. This is the admission divergence of
///   `rwlock_admission_parity`, in its `try_read` form. It happens in both variants, because the
///   `try_read` can come before the main task unlocks.
const KNOWN_DIVERGENCES: &[KnownDivergence] = &[
    KnownDivergence {
        scenario: "write+downgrade_to_upgradable | hold | upgradable_read | read",
        kinds: &[FalseDeadlock],
    },
    KnownDivergence {
        scenario: "write+downgrade_to_upgradable | hold | write | read",
        kinds: &[FalseDeadlock],
    },
    KnownDivergence {
        scenario: "write+downgrade_to_upgradable | hold | upgradable_read+upgrade | read",
        kinds: &[FalseDeadlock],
    },
    KnownDivergence {
        scenario: "upgradable_read+downgrade_upgradable | release | upgradable_read | try_read",
        kinds: &[TryResults, ShuttleOnlyValues],
    },
    KnownDivergence {
        scenario: "upgradable_read+downgrade_upgradable | hold | upgradable_read | try_read",
        kinds: &[TryResults, ShuttleOnlyValues],
    },
];

#[test]
fn transition_parity_upgrade() {
    check(TRANSITIONS[0]);
}

#[test]
fn transition_parity_try_upgrade() {
    check(TRANSITIONS[1]);
}

#[test]
fn transition_parity_downgrade() {
    check(TRANSITIONS[2]);
}

#[test]
fn transition_parity_downgrade_to_upgradable() {
    check(TRANSITIONS[3]);
}

#[test]
fn transition_parity_downgrade_upgradable() {
    check(TRANSITIONS[4]);
}

#[test]
fn transition_parity_downgrade_to_upgradable_then_upgrade() {
    check(TRANSITIONS[5]);
}

fn check(main: &[Op]) {
    let mut scenarios = Vec::new();
    for hold in [false, true] {
        for &queued in QUEUED {
            for &requested in REQUESTED {
                let mut main_program = main.to_vec();
                if hold {
                    main_program.push(AwaitSignal);
                }
                main_program.push(Unlock);

                let mut programs = vec![main_program];
                if !queued.is_empty() {
                    programs.push([queued, &[Unlock]].concat());
                }
                let signal: &[Op] = if hold { &[Signal, Unlock] } else { &[Unlock] };
                programs.push([requested, signal].concat());

                scenarios.push(Scenario {
                    columns: vec![
                        describe(main),
                        if hold { "hold" } else { "release" }.to_string(),
                        describe(queued),
                        describe(requested),
                    ],
                    programs,
                });
            }
        }
    }
    check_table(
        &["main", "variant", "queued", "requested"],
        scenarios,
        KNOWN_DIVERGENCES,
    );
}
