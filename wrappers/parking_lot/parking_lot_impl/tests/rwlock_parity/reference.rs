//! A bit-level reference model of `parking_lot` 0.12.5's `RawRwLock`, and an exhaustive explorer
//! that runs small task programs against it.
//!
//! The model keeps only the parts of the `parking_lot` state word that decide whether a request is
//! granted: `WRITER_BIT`, `UPGRADABLE_BIT`, and the reader count. Each rule below names the
//! `parking_lot` function that it copies:
//!
//! | Op                       | Granted when, and effect                                   | `parking_lot` source      |
//! |--------------------------|------------------------------------------------------------|---------------------------|
//! | `read`                   | `WRITER_BIT` is clear                                      | `try_lock_shared_fast`, `lock_shared_slow` |
//! | `upgradable_read`        | `WRITER_BIT` and `UPGRADABLE_BIT` are clear                | `lock_upgradable_slow`    |
//! | `write`, step 1          | `WRITER_BIT` and `UPGRADABLE_BIT` are clear; sets `WRITER_BIT` | `lock_exclusive_slow` |
//! | `write`, step 2          | no readers                                                 | `wait_for_readers`        |
//! | `upgrade`, step 1        | always; one reader and `UPGRADABLE_BIT` become `WRITER_BIT` | `upgrade`                |
//! | `upgrade`, step 2        | no other readers                                           | `upgrade_slow`            |
//! | `try_read`               | as `read`                                                  | `try_lock_shared_slow`    |
//! | `try_upgradable_read`    | as `upgradable_read`                                       | `try_lock_upgradable_slow` |
//! | `try_write`              | the state word is 0                                        | `try_lock_exclusive`      |
//! | `try_upgrade`            | the caller is the only reader                              | `try_upgrade_slow`        |
//! | `downgrade`              | always; `WRITER_BIT` becomes one reader                    | `downgrade`               |
//! | `downgrade_to_upgradable`| always; `WRITER_BIT` becomes one reader and `UPGRADABLE_BIT` | `downgrade_to_upgradable` |
//! | `downgrade_upgradable`   | always; clears `UPGRADABLE_BIT`                            | `downgrade_upgradable`    |
//!
//! Between its two steps, a `write` or an `upgrade` owns `WRITER_BIT` and waits for readers to
//! leave. The explorer calls this [`Holding::WriterPending`]. This is how a waiting writer blocks
//! new readers in `parking_lot`: it takes `WRITER_BIT` before it waits.
//!
//! # Why the model does not need the order in which `parking_lot` wakes tasks
//!
//! The explorer lets any task whose rule passes take the next step, in every order. `parking_lot`
//! parks blocked tasks and wakes them in a fixed order, but that does not remove any of these
//! orders. A request that `parking_lot` refuses changes no bits except `PARKED_BIT` (the writer
//! after step 1 is the exception, and the model has it as `WriterPending`). So for any order that the
//! explorer tries, `parking_lot` can give the same order: the OS delays each blocked thread until the
//! moment the explorer grants its request. A fair hand-off also grants only requests that pass these
//! rules. And `parking_lot` does not lose wake-ups, so a task that it parks runs again when its rule
//! passes. So the orders of the explorer and of `parking_lot` are the same. The test crate
//! `rwlock_reference_model` checks the rules above, and this argument, against the real
//! `parking_lot`.
//!
//! One limit: `try_write` also fails when `PARKED_BIT` is set on a free lock. That needs two or more
//! parked tasks, one of which the last unlock did not wake. The model ignores `PARKED_BIT`, and it
//! does not record which tasks a schedule refused before. So [`explore`] panics if a `try_write`
//! finds a free lock while two or more other tasks can be parked: tasks that have started and whose
//! next op is a blocking request. This includes tasks that were never refused, so the check can
//! reject a program that `parking_lot` runs without the problem. It never accepts a program that has
//! the problem. The parity tables have three tasks. The main task requests the lock only once, before
//! the other tasks start, and its later ops change the lock that it holds. So at most one other task
//! can be parked, and the tables do not reach this panic.
//!
//! # Values
//!
//! The lock guards a counter, which starts at 0. After each lock op that leaves a task holding the
//! lock, the task records the value that it sees. When the op gives the task exclusive access, the
//! task then adds 1. The outcome includes the recorded values of each task, over all schedules. This
//! shows the order in which tasks got the lock: for example, a writer that gets in while another task
//! upgrades changes the value that the upgrading task sees. The values do not show two tasks that
//! hold the lock at the same time in modes that exclude each other. The Shuttle harness checks that
//! directly (see `Outcome::overlap`).
//!
//! The counts of the model (readers, signals and the counter) are `u8`. The scenarios keep them
//! small, so [`add_one`] and [`sub_one`] panic if a count goes out of range: that is a bug in the
//! model or in a program.

use std::collections::{BTreeSet, HashSet};

/// One step of a task's program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    /// `read()`. Blocks until granted.
    Read,
    /// `upgradable_read()`. Blocks until granted.
    UpgradableRead,
    /// `write()`. Blocks until granted.
    Write,
    /// `RwLockUpgradableReadGuard::upgrade` on the upgradable guard that the task holds. Blocks until
    /// granted.
    Upgrade,
    /// `try_read()`. Records whether it succeeded.
    TryRead,
    /// `try_upgradable_read()`. Records whether it succeeded.
    TryUpgradableRead,
    /// `try_write()`. Records whether it succeeded.
    TryWrite,
    /// `RwLockUpgradableReadGuard::try_upgrade` on the upgradable guard that the task holds. Records
    /// whether it succeeded. On failure, the task keeps its upgradable guard.
    TryUpgrade,
    /// `RwLockWriteGuard::downgrade`: exclusive to shared.
    Downgrade,
    /// `RwLockWriteGuard::downgrade_to_upgradable`: exclusive to upgradable.
    DowngradeToUpgradable,
    /// `RwLockUpgradableReadGuard::downgrade`: upgradable to shared.
    DowngradeUpgradable,
    /// Drops the guard that the task holds, if any.
    Unlock,
    /// Sends one message on the scenario's channel.
    Signal,
    /// Receives one message from the scenario's channel. Blocks until a `Signal`.
    AwaitSignal,
}

impl Op {
    pub fn name(self) -> &'static str {
        match self {
            Op::Read => "read",
            Op::UpgradableRead => "upgradable_read",
            Op::Write => "write",
            Op::Upgrade => "upgrade",
            Op::TryRead => "try_read",
            Op::TryUpgradableRead => "try_upgradable_read",
            Op::TryWrite => "try_write",
            Op::TryUpgrade => "try_upgrade",
            Op::Downgrade => "downgrade",
            Op::DowngradeToUpgradable => "downgrade_to_upgradable",
            Op::DowngradeUpgradable => "downgrade_upgradable",
            Op::Unlock => "unlock",
            Op::Signal => "signal",
            Op::AwaitSignal => "await_signal",
        }
    }

    /// True for the ops that request or change the lock. After one of these, a task that holds the
    /// lock records the value.
    pub fn is_lock_op(self) -> bool {
        !matches!(self, Op::Unlock | Op::Signal | Op::AwaitSignal)
    }
}

/// What a scenario can do, over all of its schedules.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Some schedule reaches a state where no task can make progress.
    pub deadlock: bool,
    /// The results of the `try_*` ops, over all schedules. Each entry is the task, the index of the op
    /// in the task's program, and the result.
    pub try_results: BTreeSet<(usize, usize, bool)>,
    /// For each schedule that finishes, the values that each task recorded (see the module docs).
    pub values: BTreeSet<Vec<Vec<u8>>>,
    /// Some schedule lets two tasks hold the lock at the same time in modes that exclude each other.
    /// The model's rules never allow this, so only the Shuttle harness can set it.
    pub overlap: bool,
}

/// The parts of the `parking_lot` state word that decide whether a request is granted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Word {
    pub writer: bool,
    pub upgradable: bool,
    pub readers: u8,
}

/// What one task holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Holding {
    Nothing,
    /// Counts as one reader.
    Shared,
    /// Counts as one reader, and owns `UPGRADABLE_BIT`.
    Upgradable,
    /// Owns `WRITER_BIT` and waits for the readers to leave: between the two steps of a `write` or
    /// an `upgrade`. The task's program counter stays on that op.
    WriterPending,
    /// Owns `WRITER_BIT`, and no readers are left.
    Exclusive,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Task {
    pc: usize,
    holding: Holding,
    values: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    word: Word,
    value: u8,
    tasks: Vec<Task>,
    /// Messages sent and not yet received.
    signals: u8,
}

/// The result of one step.
pub struct Step {
    pub word: Word,
    pub holding: Holding,
    /// Move the program counter to the next op. False only when a `write` or an `upgrade` finishes
    /// its first step and must wait for readers.
    pub advance: bool,
    pub signals: u8,
    pub try_result: Option<bool>,
}

/// `n + 1`. Panics if `n` is at its maximum (see the module docs).
pub fn add_one(n: u8) -> u8 {
    n.checked_add(1).expect("a count of the model went above u8::MAX")
}

/// `n - 1`. Panics if `n` is 0 (see the module docs).
pub fn sub_one(n: u8) -> u8 {
    n.checked_sub(1).expect("a count of the model went below 0")
}

/// Runs `op` for a task that holds `holding`. Returns `None` if `parking_lot` blocks the task.
pub fn step(word: Word, holding: Holding, signals: u8, op: Op) -> Option<Step> {
    let done = |word, holding| Step {
        word,
        holding,
        advance: true,
        signals,
        try_result: None,
    };
    // The end of the first step of a `write` or an `upgrade`: done if no readers are left, else wait
    // for them with `WRITER_BIT` set.
    let writer_bit_taken = |word: Word| Step {
        word,
        holding: if word.readers == 0 {
            Holding::Exclusive
        } else {
            Holding::WriterPending
        },
        advance: word.readers == 0,
        signals,
        try_result: None,
    };
    let tried = |word, holding, ok| Step {
        word,
        holding,
        advance: true,
        signals,
        try_result: Some(ok),
    };
    let add_reader = |w: Word| Word {
        readers: add_one(w.readers),
        ..w
    };
    let add_upgradable = |w: Word| Word {
        readers: add_one(w.readers),
        upgradable: true,
        ..w
    };
    let can_read = !word.writer;
    let can_upgradable = !word.writer && !word.upgradable;

    match (op, holding) {
        (Op::Read, Holding::Nothing) => can_read.then(|| done(add_reader(word), Holding::Shared)),
        (Op::UpgradableRead, Holding::Nothing) => {
            can_upgradable.then(|| done(add_upgradable(word), Holding::Upgradable))
        }
        (Op::Write, Holding::Nothing) => can_upgradable.then(|| writer_bit_taken(Word { writer: true, ..word })),
        (Op::Upgrade, Holding::Upgradable) => Some(writer_bit_taken(Word {
            writer: true,
            upgradable: false,
            readers: sub_one(word.readers),
        })),
        (Op::Write | Op::Upgrade, Holding::WriterPending) => {
            (word.readers == 0).then(|| done(word, Holding::Exclusive))
        }

        (Op::TryRead, Holding::Nothing) => Some(if can_read {
            tried(add_reader(word), Holding::Shared, true)
        } else {
            tried(word, Holding::Nothing, false)
        }),
        (Op::TryUpgradableRead, Holding::Nothing) => Some(if can_upgradable {
            tried(add_upgradable(word), Holding::Upgradable, true)
        } else {
            tried(word, Holding::Nothing, false)
        }),
        (Op::TryWrite, Holding::Nothing) => Some(if word == Word::default() {
            tried(Word { writer: true, ..word }, Holding::Exclusive, true)
        } else {
            tried(word, Holding::Nothing, false)
        }),
        (Op::TryUpgrade, Holding::Upgradable) => Some(if word.readers == 1 {
            tried(
                Word {
                    writer: true,
                    upgradable: false,
                    readers: 0,
                },
                Holding::Exclusive,
                true,
            )
        } else {
            tried(word, Holding::Upgradable, false)
        }),

        (Op::Downgrade, Holding::Exclusive) => Some(done(
            Word {
                writer: false,
                readers: add_one(word.readers),
                ..word
            },
            Holding::Shared,
        )),
        (Op::DowngradeToUpgradable, Holding::Exclusive) => Some(done(
            Word {
                writer: false,
                upgradable: true,
                readers: add_one(word.readers),
            },
            Holding::Upgradable,
        )),
        (Op::DowngradeUpgradable, Holding::Upgradable) => Some(done(
            Word {
                upgradable: false,
                ..word
            },
            Holding::Shared,
        )),

        (Op::Unlock, holding) => Some(done(
            match holding {
                Holding::Nothing => word,
                Holding::Shared => Word {
                    readers: sub_one(word.readers),
                    ..word
                },
                Holding::Upgradable => Word {
                    readers: sub_one(word.readers),
                    upgradable: false,
                    ..word
                },
                Holding::Exclusive => Word { writer: false, ..word },
                Holding::WriterPending => unreachable!("a pending writer is blocked, so it cannot unlock"),
            },
            Holding::Nothing,
        )),
        (Op::Signal, holding) => Some(Step {
            signals: add_one(signals),
            ..done(word, holding)
        }),
        (Op::AwaitSignal, holding) => (signals > 0).then(|| Step {
            signals: sub_one(signals),
            ..done(word, holding)
        }),

        (op, holding) => panic!("invalid program: {} while holding {holding:?}", op.name()),
    }
}

/// True if the task can be parked in `parking_lot`: it has started, and its next op is a blocking
/// request. The model does not record whether a schedule refused the request before, so this is true
/// also of a task that never parked (see the module docs).
fn may_be_parked(started: bool, task: &Task, program: &[Op]) -> bool {
    let blocking_request = |op| matches!(op, Op::Read | Op::UpgradableRead | Op::Write | Op::Upgrade);
    started && program.get(task.pc).is_some_and(|&op| blocking_request(op))
}

/// Explores every schedule of `programs` against the reference model.
///
/// Task 0 runs its first op alone. Then the other tasks start, and all tasks run to the end of their
/// programs. This is the same order as the Shuttle harness, where the main task takes the lock and
/// then spawns the other tasks.
pub fn explore(programs: &[Vec<Op>]) -> Outcome {
    let initial = State {
        word: Word::default(),
        value: 0,
        tasks: vec![
            Task {
                pc: 0,
                holding: Holding::Nothing,
                values: Vec::new(),
            };
            programs.len()
        ],
        signals: 0,
    };
    let mut outcome = Outcome::default();
    let mut seen = HashSet::new();
    let mut stack = vec![initial];

    while let Some(state) = stack.pop() {
        if !seen.insert(state.clone()) {
            continue;
        }
        let spawned = state.tasks[0].pc >= 1;
        let mut finished = true;
        let mut progress = false;

        for (i, task) in state.tasks.iter().enumerate() {
            let Some(&op) = programs[i].get(task.pc) else {
                continue;
            };
            finished = false;
            if i > 0 && !spawned {
                continue;
            }
            let Some(next) = step(state.word, task.holding, state.signals, op) else {
                continue;
            };
            if op == Op::TryWrite && state.word == Word::default() {
                let parked = (state.tasks.iter().enumerate())
                    .filter(|&(j, other)| j != i && may_be_parked(j == 0 || spawned, other, &programs[j]))
                    .count();
                assert!(
                    parked < 2,
                    "outside the model: try_write on a free lock while {parked} other tasks can be parked \
                     (parking_lot can fail it because of PARKED_BIT)"
                );
            }
            progress = true;
            if let Some(ok) = next.try_result {
                outcome.try_results.insert((i, task.pc, ok));
            }

            let mut successor = state.clone();
            successor.word = next.word;
            successor.signals = next.signals;
            let t = &mut successor.tasks[i];
            if op.is_lock_op() && next.advance && next.holding != Holding::Nothing {
                t.values.push(state.value);
                if next.holding == Holding::Exclusive && task.holding != Holding::Exclusive {
                    successor.value = add_one(successor.value);
                }
            }
            t.pc += usize::from(next.advance);
            t.holding = next.holding;
            stack.push(successor);
        }

        if finished {
            outcome
                .values
                .insert(state.tasks.iter().map(|t| t.values.clone()).collect());
        } else if !progress {
            outcome.deadlock = true;
        }
    }
    outcome
}
