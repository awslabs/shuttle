//! An exact model of `parking_lot` 0.12.5's `RawRwLock`, with its park queue, and an exhaustive
//! explorer that runs task programs against it.
//!
//! The rules of [`super::reference`] decide which requests the lock word admits, and its explorer
//! lets a request in as soon as the word admits it. Those are all the orders in which
//! `parking_lot` can grant the requests, but `parking_lot` has more states: a request that the word
//! refuses parks, and tries again only when an unlock wakes it, which is not always when the word
//! admits it. This model adds the park queue, and with it what the queue makes observable. An
//! unlock can leave a reader parked that an upgradable reader, which takes the lock later, admits.
//! A program in which the upgradable reader then waits for that reader deadlocks (the "stranded
//! reader" queue scenario of `rwlock_parity::programs`). And `try_write` can fail on a free lock,
//! because a request is parked. The model keeps:
//!
//! * The main queue, in the order in which the requests park: `parking_lot_core::park` appends to
//!   the back.
//! * A request that the word refuses parks at the back, in the same step as its attempt.
//!   `parking_lot` sets `PARKED_BIT` and then parks, but `park` checks the word again under the
//!   lock of the queue (`validate`), and a request that the word now admits tries again instead.
//!   So no unlock comes between the refusal and the park.
//! * A parked task does not run until a release takes it out of the queue.
//! * The releases that wake the queue ([`wake_of`]), and which requests each one wakes: the filter
//!   of `wake_parked_threads` ([`wake_filter`], copied line by line), from the state that the
//!   release starts it with.
//! * A woken request tries again like a new request. If the word refuses it, it parks again, at
//!   the back.
//! * `unlock_fair` of a write lock hands the lock to the requests that it wakes. The model grants
//!   them in queue order, which ends in the state that `parking_lot` stores for them. Every other
//!   release lets the woken requests race with the other tasks. That includes `unlock_fair` of an
//!   upgradable read lock, to which `parking_lot` 0.12.5 passes `force_fair = false`.
//! * `try_write` fails while the queue is not empty: it is a compare-and-swap of the word from 0,
//!   which fails while `PARKED_BIT` is set. The other `try_*` methods ignore the bit.
//! * Shared unlocks never wake the main queue. The last reader to leave wakes only a writer that
//!   waits for the readers, which the model has as [`Holding::WriterPending`]: such a writer is
//!   not in the main queue, and goes on as soon as no reader is left.
//!
//! It leaves out what cannot change the outcomes of a program:
//!
//! * Eventual fairness. `parking_lot` makes a plain unlock hand the lock off now and then, on a
//!   timer. A hand-off ends in the state that the woken requests reach by trying again at once, in
//!   queue order, so it removes outcomes and adds none. And every plain unlock can be plain.
//! * Spinning, and the delays of the OS, which only change the order of the steps. The explorer
//!   tries every order.
//! * Timed requests and the `bump` methods, which the programs do not use.
//!
//! `rwlock_reference_model` checks this model against the real `parking_lot`: the filter, on
//! every queue of up to four requests, and the results of programs on OS threads.
//! `rwlock_exact_parity` checks Shuttle's lock against it.
//!
//! # Overtakes, for Shuttle's fairness bound
//!
//! Shuttle's lock leaves out some of these schedules on purpose. In place of the timer, a woken
//! request that has been overtaken `Config::parking_lot_fairness_bound` times is let in at the next
//! release, oldest request first (see `Fairness::ParkingLot`). The engine counts as overtakes of a
//! request the races that it lost after a release woke it, and the releases that passed it while
//! it was woken: every release while it is woken, except the one that woke it. It counts them for
//! as long as the request waits, across its parks. The model counts the same events, and
//! [`Exploration::max_overtakes`] is the most that any woken request has right after a release,
//! over all schedules. If that is less than the bound, Shuttle never lets a request in early, and
//! must reach every outcome of the model. The programs make no other calls that the engine counts
//! (`BatchSemaphore::load_permits`, as in `is_locked`, or the `bump` methods).

use super::reference::{Holding, Op, Outcome, Step, Word, add_one, step};
use std::collections::HashSet;

/// `parking_lot`'s `UPGRADABLE_BIT`.
pub const UPGRADABLE_BIT: usize = 0b0100;
/// `parking_lot`'s `WRITER_BIT`.
pub const WRITER_BIT: usize = 0b1000;
/// `parking_lot`'s `ONE_READER`, the unit of the reader count.
pub const ONE_READER: usize = 0b10000;

/// The token with which a request parks. [`Token::value`] is the value of `parking_lot`'s park
/// token for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Token {
    /// `read` (`TOKEN_SHARED`).
    Shared,
    /// `upgradable_read` (`TOKEN_UPGRADABLE`).
    Upgradable,
    /// `write` (`TOKEN_EXCLUSIVE`).
    Exclusive,
}

impl Token {
    pub const ALL: [Token; 3] = [Token::Shared, Token::Upgradable, Token::Exclusive];

    /// The value of `parking_lot`'s park token: `TOKEN_SHARED`, `TOKEN_UPGRADABLE` or
    /// `TOKEN_EXCLUSIVE`.
    pub const fn value(self) -> usize {
        match self {
            Token::Shared => ONE_READER,
            Token::Upgradable => ONE_READER | UPGRADABLE_BIT,
            Token::Exclusive => WRITER_BIT,
        }
    }

    /// The token of `op`, if `op` is a request that parks when the word refuses it. An `upgrade`
    /// never parks in the main queue: it takes `WRITER_BIT` at once, and then waits for the
    /// readers to leave.
    pub fn of(op: Op) -> Option<Token> {
        match op {
            Op::Read => Some(Token::Shared),
            Op::UpgradableRead => Some(Token::Upgradable),
            Op::Write => Some(Token::Exclusive),
            _ => None,
        }
    }
}

/// The filter of `parking_lot`'s `RawRwLock::wake_parked_threads`, copied line by line: the
/// indices of the parked requests that a release unparks, given the values of their park tokens in
/// queue order, front first, and the state `start` that the release starts the filter with.
///
/// From the front, it unparks readers up to and including the first writer. Once it has unparked
/// an upgradable reader, it skips writers and upgradable readers, and goes on unparking readers.
pub fn wake_filter(tokens: &[usize], start: usize) -> Vec<usize> {
    let mut new_state = start;
    let mut unparked = Vec::new();
    for (i, &token) in tokens.iter().enumerate() {
        let s = new_state;
        // If we are waking up a writer, don't wake anything else.
        if s & WRITER_BIT != 0 {
            break; // FilterOp::Stop
        }
        // Otherwise wake *all* readers and one upgrader/writer.
        if token & (UPGRADABLE_BIT | WRITER_BIT) != 0 && s & UPGRADABLE_BIT != 0 {
            // Skip writers and upgradable readers if we already have a writer/upgradable reader.
            continue; // FilterOp::Skip
        }
        new_state = s + token;
        unparked.push(i); // FilterOp::Unpark
    }
    unparked
}

/// How a release wakes the main queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wake {
    /// The state that the release starts [`wake_filter`] with.
    pub start: usize,
    /// Whether the release hands the lock to the requests that it wakes.
    pub hand_off: bool,
}

/// How `op`, run by a task that holds `holding`, wakes the main queue, or `None` if it wakes no
/// request in it. Each of these releases wakes the queue only while `PARKED_BIT` is set, that is,
/// while the queue is not empty.
///
/// | Release                                       | Starts the filter with         | Hands off |
/// |-----------------------------------------------|--------------------------------|-----------|
/// | `unlock` of a write lock                      | 0                              | no        |
/// | `unlock_fair` of a write lock                 | 0                              | yes       |
/// | `unlock`, `unlock_fair` of an upgradable read | 0                              | no        |
/// | `downgrade`, `downgrade_upgradable`           | `ONE_READER`                   | no        |
/// | `downgrade_to_upgradable`                     | `ONE_READER \| UPGRADABLE_BIT` | no        |
/// | `unlock`, `unlock_fair` of a read lock        | does not wake the main queue   |           |
pub fn wake_of(op: Op, holding: Holding) -> Option<Wake> {
    let wake = |start, hand_off| Some(Wake { start, hand_off });
    match (op, holding) {
        (Op::Unlock, Holding::Exclusive) => wake(0, false),
        (Op::UnlockFair, Holding::Exclusive) => wake(0, true),
        // `unlock_upgradable_fair` passes `force_fair = false` to `unlock_upgradable_slow`.
        (Op::Unlock | Op::UnlockFair, Holding::Upgradable) => wake(0, false),
        (Op::Downgrade, Holding::Exclusive) | (Op::DowngradeUpgradable, Holding::Upgradable) => wake(ONE_READER, false),
        (Op::DowngradeToUpgradable, Holding::Exclusive) => wake(ONE_READER | UPGRADABLE_BIT, false),
        _ => None,
    }
}

/// Whether `op`, run by a task that holds `holding`, gives up the lock or part of it. Each of these
/// is a release of Shuttle's semaphore, which overtakes the woken requests (see the module docs).
fn is_release(op: Op, holding: Holding) -> bool {
    match op {
        Op::Unlock | Op::UnlockFair => holding != Holding::Nothing,
        Op::Downgrade | Op::DowngradeToUpgradable | Op::DowngradeUpgradable => true,
        _ => false,
    }
}

/// What the explorer finds over all schedules of a program.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exploration {
    pub outcome: Outcome,
    /// The most overtakes that a request has while it is woken, right after a release, over all
    /// schedules (see the module docs).
    pub max_overtakes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Task {
    pc: usize,
    holding: Holding,
    values: Vec<u8>,
    /// The request of the next op is in the queue.
    parked: bool,
    /// A release took the request out of the queue, without a hand-off, and it has not tried again
    /// yet.
    woken: bool,
    /// The overtakes of the request of the next op (see the module docs). 0 when it is not
    /// waiting.
    overtakes: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    word: Word,
    value: u8,
    tasks: Vec<Task>,
    /// The parked requests, front first: the task and its token.
    queue: Vec<(usize, Token)>,
    /// Messages sent and not yet received.
    signals: u8,
}

/// Applies `next`, a step that the rules allow for task `i`'s op `op`, to `state`, with the value
/// rule of the reference model (see its module docs). The step ends the request of the op, if it
/// was one: it was granted, or a `write` took `WRITER_BIT` and waits for the readers.
fn apply(state: &mut State, i: usize, op: Op, next: &Step) {
    let before = state.tasks[i].holding;
    state.word = next.word;
    state.signals = next.signals;
    let task = &mut state.tasks[i];
    if op.is_lock_op() && next.advance && next.holding != Holding::Nothing {
        task.values.push(state.value);
        if next.holding == Holding::Exclusive && before != Holding::Exclusive {
            state.value = add_one(state.value);
        }
    }
    task.pc += usize::from(next.advance);
    task.holding = next.holding;
    task.woken = false;
    task.overtakes = 0;
}

/// After a release, which `apply` has applied: the release overtakes the requests that are woken,
/// and then wakes the requests that `wake` unparks, or hands them the lock. Returns the most
/// overtakes of a woken request after the release.
fn release(state: &mut State, programs: &[Vec<Op>], wake: Option<Wake>) -> usize {
    // A release passes the requests that are woken already, but not the ones that it wakes.
    for task in state.tasks.iter_mut().filter(|t| t.woken) {
        task.overtakes = add_one(task.overtakes);
    }
    if let Some(wake) = wake {
        let tokens: Vec<usize> = state.queue.iter().map(|&(_, token)| token.value()).collect();
        let unparked: Vec<usize> = (wake_filter(&tokens, wake.start).into_iter())
            .map(|q| state.queue[q].0)
            .collect();
        // A task has at most one request, so it is in the queue at most once.
        state.queue.retain(|(task, _)| !unparked.contains(task));
        for j in unparked {
            state.tasks[j].parked = false;
            if wake.hand_off {
                // In queue order, so that the readers come before the writer that ends the set.
                let op = programs[j][state.tasks[j].pc];
                let granted = step(state.word, state.tasks[j].holding, state.signals, op)
                    .expect("a hand-off grants only requests that the rules admit");
                apply(state, j, op, &granted);
            } else {
                state.tasks[j].woken = true;
            }
        }
    }
    let woken = state.tasks.iter().filter(|t| t.woken);
    woken.map(|t| usize::from(t.overtakes)).max().unwrap_or(0)
}

/// Explores every schedule of `programs` against the exact model.
///
/// Task 0 runs its first op alone. Then the other tasks start, and all tasks run to the end of
/// their programs. This is the same order as the Shuttle harness, where the main task takes the
/// lock and then spawns the other tasks.
pub fn explore_exact(programs: &[Vec<Op>]) -> Exploration {
    let task = Task {
        pc: 0,
        holding: Holding::Nothing,
        values: Vec::new(),
        parked: false,
        woken: false,
        overtakes: 0,
    };
    let initial = State {
        word: Word::default(),
        value: 0,
        tasks: vec![task; programs.len()],
        queue: Vec::new(),
        signals: 0,
    };
    let mut exploration = Exploration::default();
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
            if (i > 0 && !spawned) || task.parked {
                continue;
            }
            // `try_write` is a compare-and-swap of the word from 0, so it also fails while a
            // request is parked (`PARKED_BIT`).
            let next = if op == Op::TryWrite && !state.queue.is_empty() {
                Some(Step {
                    word: state.word,
                    holding: task.holding,
                    advance: true,
                    signals: state.signals,
                    try_result: Some(false),
                })
            } else {
                step(state.word, task.holding, state.signals, op)
            };
            let mut successor = state.clone();
            match next {
                Some(next) => {
                    if let Some(ok) = next.try_result {
                        exploration.outcome.try_results.insert((i, task.pc, ok));
                    }
                    apply(&mut successor, i, op, &next);
                    if is_release(op, task.holding) {
                        let overtakes = release(&mut successor, programs, wake_of(op, task.holding));
                        exploration.max_overtakes = exploration.max_overtakes.max(overtakes);
                    }
                }
                None => {
                    // Refused. A request parks at the back; if a release woke it, it lost the race
                    // that the release woke it for. A pending writer, and a task that waits for a
                    // signal, wait without a step.
                    let Some(token) = Token::of(op).filter(|_| task.holding == Holding::Nothing) else {
                        continue;
                    };
                    let task = &mut successor.tasks[i];
                    if task.woken {
                        task.woken = false;
                        task.overtakes = add_one(task.overtakes);
                    }
                    task.parked = true;
                    successor.queue.push((i, token));
                }
            }
            progress = true;
            stack.push(successor);
        }

        if finished {
            let values = state.tasks.iter().map(|t| t.values.clone()).collect();
            exploration.outcome.values.insert(values);
        } else if !progress {
            exploration.outcome.deadlock = true;
        }
    }
    exploration
}
