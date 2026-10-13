//! Runs a task's program on a real lock: Shuttle's `RwLock` in the harness, or `parking_lot`'s in
//! `rwlock_reference_model`. Both are `lock_api::RwLock<R, u8>`, so one [`Actor`] serves both, and the
//! value rule of the reference model (see its module docs) has one copy for the real locks.

use super::reference::Op::{self, *};
use super::reference::{Holding, add_one};
use lock_api::{
    RawRwLockFair, RawRwLockUpgradeDowngrade, RawRwLockUpgradeFair, RwLock, RwLockReadGuard, RwLockUpgradableReadGuard,
    RwLockWriteGuard,
};

/// The raw-lock abilities that the programs use: upgrades, downgrades, and fair unlocks. Both
/// Shuttle's `RawRwLock` and `parking_lot`'s have them all.
pub trait RawLock: RawRwLockUpgradeDowngrade + RawRwLockFair + RawRwLockUpgradeFair {}
impl<R: RawRwLockUpgradeDowngrade + RawRwLockFair + RawRwLockUpgradeFair> RawLock for R {}

/// The guard that a task holds.
pub enum Guard<'a, R: RawLock> {
    Nothing,
    Read(RwLockReadGuard<'a, R, u8>),
    Upgradable(RwLockUpgradableReadGuard<'a, R, u8>),
    Write(RwLockWriteGuard<'a, R, u8>),
}

impl<'a, R: RawLock> Guard<'a, R> {
    /// The result of a `try_*` op: the new guard, and whether the op succeeded.
    pub fn tried<G>(g: Option<G>, wrap: fn(G) -> Self) -> (Self, Option<bool>) {
        match g {
            Some(g) => (wrap(g), Some(true)),
            None => (Guard::Nothing, Some(false)),
        }
    }

    /// Runs `op` on `lock`, where `self` is the guard that the task holds. A blocking request blocks
    /// until it is granted. Returns the new guard and, for a `try_*` op, whether the op succeeded.
    /// `Signal` and `AwaitSignal` do not change the guard: the caller sends and receives, because the
    /// channel depends on the runtime.
    pub fn apply(self, lock: &'a RwLock<R, u8>, op: Op) -> (Self, Option<bool>) {
        match (op, self) {
            (Read, Guard::Nothing) => (Guard::Read(lock.read()), None),
            (UpgradableRead, Guard::Nothing) => (Guard::Upgradable(lock.upgradable_read()), None),
            (Write, Guard::Nothing) => (Guard::Write(lock.write()), None),
            (Upgrade, Guard::Upgradable(g)) => (Guard::Write(RwLockUpgradableReadGuard::upgrade(g)), None),
            (TryRead, Guard::Nothing) => Self::tried(lock.try_read(), Guard::Read),
            (TryUpgradableRead, Guard::Nothing) => Self::tried(lock.try_upgradable_read(), Guard::Upgradable),
            (TryWrite, Guard::Nothing) => Self::tried(lock.try_write(), Guard::Write),
            (TryUpgrade, Guard::Upgradable(g)) => match RwLockUpgradableReadGuard::try_upgrade(g) {
                Ok(g) => (Guard::Write(g), Some(true)),
                Err(g) => (Guard::Upgradable(g), Some(false)),
            },
            (Downgrade, Guard::Write(g)) => (Guard::Read(RwLockWriteGuard::downgrade(g)), None),
            (DowngradeToUpgradable, Guard::Write(g)) => {
                (Guard::Upgradable(RwLockWriteGuard::downgrade_to_upgradable(g)), None)
            }
            (DowngradeUpgradable, Guard::Upgradable(g)) => (Guard::Read(RwLockUpgradableReadGuard::downgrade(g)), None),
            (Unlock, _) => (Guard::Nothing, None),
            (UnlockFair, Guard::Read(g)) => {
                RwLockReadGuard::unlock_fair(g);
                (Guard::Nothing, None)
            }
            (UnlockFair, Guard::Upgradable(g)) => {
                RwLockUpgradableReadGuard::unlock_fair(g);
                (Guard::Nothing, None)
            }
            (UnlockFair, Guard::Write(g)) => {
                RwLockWriteGuard::unlock_fair(g);
                (Guard::Nothing, None)
            }
            (UnlockFair, Guard::Nothing) => (Guard::Nothing, None),
            (Signal | AwaitSignal, guard) => (guard, None),
            (op, _) => panic!("invalid program: {} with the wrong guard", op.name()),
        }
    }

    /// The value that the task sees, if it holds the lock.
    fn value(&self) -> Option<u8> {
        match self {
            Guard::Nothing => None,
            Guard::Read(g) => Some(**g),
            Guard::Upgradable(g) => Some(**g),
            Guard::Write(g) => Some(**g),
        }
    }
}

/// Runs one task's program on a real lock, and records values and `try_*` results with the same
/// rules as the reference model.
pub struct Actor<'a, R: RawLock> {
    lock: &'a RwLock<R, u8>,
    guard: Guard<'a, R>,
    /// The index of the next op in the task's program.
    pc: usize,
    /// The values that this task recorded (see the reference module docs).
    pub values: Vec<u8>,
    /// The index in the program and the result of each `try_*` op that this task ran.
    pub tries: Vec<(usize, bool)>,
}

impl<'a, R: RawLock> Actor<'a, R> {
    pub fn new(lock: &'a RwLock<R, u8>) -> Self {
        Self {
            lock,
            guard: Guard::Nothing,
            pc: 0,
            values: Vec::new(),
            tries: Vec::new(),
        }
    }

    /// What the task holds. A guard cannot show `Holding::WriterPending`, so this never returns it.
    pub fn holding(&self) -> Holding {
        match self.guard {
            Guard::Nothing => Holding::Nothing,
            Guard::Read(_) => Holding::Shared,
            Guard::Upgradable(_) => Holding::Upgradable,
            Guard::Write(_) => Holding::Exclusive,
        }
    }

    /// Runs the task's next op, `op`. For `Signal` and `AwaitSignal`, the caller sends or receives
    /// first (see [`Guard::apply`]).
    pub fn step(&mut self, op: Op) {
        // The value rule of the reference model: record the value after each lock op that leaves the
        // task holding the lock, then add 1 on getting exclusive access.
        let was_exclusive = matches!(self.guard, Guard::Write(_));
        let (guard, ok) = std::mem::replace(&mut self.guard, Guard::Nothing).apply(self.lock, op);
        self.guard = guard;
        self.tries.extend(ok.map(|ok| (self.pc, ok)));
        if op.is_lock_op() {
            self.values.extend(self.guard.value());
            if let Guard::Write(g) = &mut self.guard
                && !was_exclusive
            {
                **g = add_one(**g);
            }
        }
        self.pc += 1;
    }
}
