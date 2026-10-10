//! Fair-unlock parity: when the lock is released with `unlock_fair`, does Shuttle's `RwLock` still
//! behave like `parking_lot`?
//!
//! Shuttle's fair unlock hands the lock to the waiting tasks inside the release (see
//! `BatchSemaphore::with_fair_releases`), which removes schedules rather than adding them: no request can
//! overtake the hand-off. The reference model keeps every order, because `parking_lot` reaches each
//! of them by delaying when its tasks park (see "Why the model does not need the order in which
//! `parking_lot` wakes tasks" in the reference module docs). So what this table pins down is that
//! the hand-off loses no outcome: a `MissedValues` or `MissedDeadlock` divergence here would mean
//! that Shuttle's hand-off is stricter than any order of parking that `parking_lot` can give. The
//! stress test of `rwlock_reference_model` checks the model's side on the real `parking_lot`.
//!
//! The scenarios are the admission table's, with one change: the main task releases its lock after
//! the signal (as in the `release` variant of the transition table), and the main and queued tasks
//! unlock fairly, so the fair unlocks happen while requests can be queued behind them.
//!
//! [`KNOWN_DIVERGENCES`] lists the scenarios where the Shuttle model is known to be wrong. A new
//! divergence fails the test. So does a known divergence that no longer happens: remove it from the
//! list when the model is fixed. Run with `--nocapture` to see the full table.

// Each test crate uses only part of the shared module.
#[allow(dead_code)]
mod rwlock_parity;

use rwlock_parity::harness::{KnownDivergence, QUEUED, REQUESTED, Scenario, check_table, describe};
use rwlock_parity::reference::Op::{self, *};

/// The scenarios where the Shuttle model is known to be wrong, with the reason for each. There are
/// none: Shuttle matches the reference model in every scenario of this table.
const KNOWN_DIVERGENCES: &[KnownDivergence] = &[];

#[test]
fn fair_unlock_parity_after_read_held() {
    check(Read);
}

#[test]
fn fair_unlock_parity_after_upgradable_read_held() {
    check(UpgradableRead);
}

#[test]
fn fair_unlock_parity_after_write_held() {
    check(Write);
}

fn check(held: Op) {
    let mut scenarios = Vec::new();
    for &queued in QUEUED {
        for &requested in REQUESTED {
            let mut programs = vec![vec![held, AwaitSignal, UnlockFair]];
            if !queued.is_empty() {
                programs.push([queued, &[UnlockFair]].concat());
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
