//! Admission parity: does Shuttle's `RwLock` grant and refuse the same requests as `parking_lot`?
//!
//! Each scenario has three tasks on one lock:
//!
//! * The main task takes the lock in the `held` mode. Then it spawns the other two tasks and keeps
//!   the lock until the third task sends a signal.
//! * The second task makes the `queued` requests, then unlocks. It gives the context: a request that
//!   waits behind the main task (for example a `write` behind an `upgradable_read`), or a request
//!   that is granted and released at once.
//! * The third task makes the `requested` requests, sends the signal, then unlocks.
//!
//! So a deadlock is possible exactly when some schedule can refuse the third task's request while
//! the main task holds the lock. The scenarios are the full cross product of `held` and the
//! `QUEUED` and `REQUESTED` lists in the harness. For each one, the harness compares the reference
//! model of `parking_lot` 0.12.5 with Shuttle (see `rwlock_parity::harness::Kind` for the rules).
//!
//! [`KNOWN_DIVERGENCES`] lists the scenarios where the Shuttle model is known to be wrong. A new
//! divergence fails the test. So does a known divergence that no longer happens: remove it from the
//! list when the model is fixed.
//!
//! Run with `--nocapture` to see the full table. `rwlock_transition_parity` covers upgrades and
//! downgrades by the main task. Neither table covers `read_recursive` or the timed requests, because
//! this crate does not implement `RawRwLockRecursive` or `RawRwLockTimed`.

// Each test crate uses only part of the shared module.
#[allow(dead_code)]
mod rwlock_parity;

use rwlock_parity::harness::Kind::*;
use rwlock_parity::harness::{KnownDivergence, QUEUED, REQUESTED, Scenario, check_table, describe};
use rwlock_parity::reference::Op::{self, *};

/// All the known divergences are in one cell of the block matrix: an upgradable read is held, and a
/// plain read is requested. `parking_lot` grants the read, because only `WRITER_BIT` blocks a plain
/// reader, and a task that waits behind an upgradable read does not own `WRITER_BIT`. Shuttle
/// refuses it, because each lock state is a permit request on one strictly fair `BatchSemaphore`,
/// and that semaphore refuses every new request while a waiter is queued.
///
/// The queued task can be a `write`, an `upgradable_read`, or an `upgradable_read` followed by an
/// `upgrade`. Each one blocks the reader.
///
/// With `read` the result is a false deadlock. With `try_read` the result is a false failure, and
/// the third task then records no value, which no `parking_lot` schedule does.
const KNOWN_DIVERGENCES: &[KnownDivergence] = &[
    KnownDivergence {
        scenario: "upgradable_read | upgradable_read | read",
        kinds: &[FalseDeadlock],
    },
    KnownDivergence {
        scenario: "upgradable_read | upgradable_read | try_read",
        kinds: &[TryResults, ShuttleOnlyValues],
    },
    KnownDivergence {
        scenario: "upgradable_read | write | read",
        kinds: &[FalseDeadlock],
    },
    KnownDivergence {
        scenario: "upgradable_read | write | try_read",
        kinds: &[TryResults, ShuttleOnlyValues],
    },
    KnownDivergence {
        scenario: "upgradable_read | upgradable_read+upgrade | read",
        kinds: &[FalseDeadlock],
    },
    KnownDivergence {
        scenario: "upgradable_read | upgradable_read+upgrade | try_read",
        kinds: &[TryResults, ShuttleOnlyValues],
    },
];

#[test]
fn admission_parity_while_read_held() {
    check(Read);
}

#[test]
fn admission_parity_while_upgradable_read_held() {
    check(UpgradableRead);
}

#[test]
fn admission_parity_while_write_held() {
    check(Write);
}

fn check(held: Op) {
    let mut scenarios = Vec::new();
    for &queued in QUEUED {
        for &requested in REQUESTED {
            let mut programs = vec![vec![held, AwaitSignal, Unlock]];
            if !queued.is_empty() {
                programs.push([queued, &[Unlock]].concat());
            }
            programs.push([requested, &[Signal, Unlock]].concat());
            scenarios.push(Scenario {
                columns: vec![held.name().to_string(), describe(queued), describe(requested)],
                programs,
            });
        }
    }
    check_table(&["held", "queued", "requested"], scenarios, KNOWN_DIVERGENCES);
}
