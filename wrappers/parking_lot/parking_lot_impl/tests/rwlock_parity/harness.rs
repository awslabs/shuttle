//! Runs parity scenarios on Shuttle's `RwLock` and on the reference model, and compares the results.
//!
//! A scenario is one program for each task. Task 0 is the main task: it runs its first op, then
//! spawns the other tasks, then runs the rest of its program and joins them. [`explore`] tries every
//! schedule of the programs against the reference model, and [`run_shuttle`] tries every schedule
//! with Shuttle's DFS scheduler. Both sides record values and `try_*` results with the same rules
//! (the model in `explore`, the real lock in `actor::Actor`). [`check_table`] compares the two for a
//! list of scenarios. The comparison rules are on [`Kind`].

use super::actor::Actor;
use super::reference::{Holding, Op, Outcome, explore};
use shuttle::scheduler::DfsScheduler;
use shuttle::{
    Config, FailurePersistence, Runner,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};
use shuttle_parking_lot_impl::{RawRwLock, RwLock};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use Op::*;

/// The start of the panic message with which Shuttle reports a deadlock (`StepError::Deadlock` in
/// `shuttle-engine/src/runtime/execution.rs`). [`run_shuttle`] records this panic as a deadlock, and
/// passes on all other panics.
const DEADLOCK_PANIC: &str = "deadlock! blocked tasks";

/// The second task's requests in each table. Empty means that there is no second task.
pub const QUEUED: &[&[Op]] = &[&[], &[Read], &[UpgradableRead], &[Write], &[UpgradableRead, Upgrade]];

/// The third task's requests in each table.
pub const REQUESTED: &[&[Op]] = &[
    &[Read],
    &[UpgradableRead],
    &[Write],
    &[UpgradableRead, Upgrade],
    &[TryRead],
    &[TryUpgradableRead],
    &[TryWrite],
    &[UpgradableRead, TryUpgrade],
];

/// The main task's programs in the transition table, before its final `unlock`. Each one targets a
/// transition:
///
/// * `upgrade`: no writer can get in between the upgradable read and the write.
/// * `try_upgrade`: succeeds while a writer is queued.
/// * `downgrade`: readers get in, and a queued writer takes `WRITER_BIT` and blocks new readers.
/// * `downgrade_to_upgradable`: never blocks, and admits readers.
/// * `downgrade_upgradable`: admits a queued `upgradable_read` or `write`.
/// * `downgrade_to_upgradable` then `upgrade`: the round trip.
pub const TRANSITIONS: &[&[Op]] = &[
    &[UpgradableRead, Upgrade],
    &[UpgradableRead, TryUpgrade],
    &[Write, Downgrade],
    &[Write, DowngradeToUpgradable],
    &[UpgradableRead, DowngradeUpgradable],
    &[Write, DowngradeToUpgradable, Upgrade],
];

/// One way in which Shuttle's result can differ from the reference model's result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Shuttle finds a deadlock, and `parking_lot` cannot deadlock.
    FalseDeadlock,
    /// `parking_lot` can deadlock, and Shuttle does not find it.
    MissedDeadlock,
    /// The `try_*` results differ for some op of some task (see [`Outcome::try_results`]). Compared
    /// only when neither side deadlocks, because Shuttle stops at its first deadlock.
    TryResults,
    /// A schedule in Shuttle records values that no `parking_lot` schedule can record. Always
    /// compared.
    ShuttleOnlyValues,
    /// A `parking_lot` schedule records values that no Shuttle schedule records, so Shuttle does not
    /// test that behavior. Compared only when neither side deadlocks.
    MissedValues,
    /// Shuttle lets two tasks hold the lock at the same time in modes that exclude each other (see
    /// [`Outcome::overlap`]). Always compared: the model never allows it.
    Overlap,
}

/// A scenario where Shuttle is known to differ from `parking_lot`, and how.
pub struct KnownDivergence {
    /// The scenario's columns, joined with `" | "`, as the table prints them.
    pub scenario: &'static str,
    pub kinds: &'static [Kind],
}

/// One row of a table: the columns that name the scenario, and the task programs.
pub struct Scenario {
    pub columns: Vec<String>,
    pub programs: Vec<Vec<Op>>,
}

pub fn describe(ops: &[Op]) -> String {
    if ops.is_empty() {
        "-".to_string()
    } else {
        ops.iter().map(|op| op.name()).collect::<Vec<_>>().join("+")
    }
}

/// Describes an outcome in a table cell. `try 2.0 {..}` gives the results of op 0 of task 2.
fn describe_outcome(outcome: &Outcome) -> String {
    let mut s = if outcome.deadlock { "deadlock" } else { "ok" }.to_string();
    if outcome.overlap {
        s.push_str(", overlap");
    }
    let mut tries = BTreeMap::<_, BTreeSet<bool>>::new();
    for &(task, op, ok) in &outcome.try_results {
        tries.entry((task, op)).or_default().insert(ok);
    }
    for ((task, op), results) in tries {
        write!(s, ", try {task}.{op} {results:?}").unwrap();
    }
    write!(s, ", {} value sets", outcome.values.len()).unwrap();
    s
}

/// How Shuttle's result `actual` differs from the reference model's result `expected`.
pub fn divergences(expected: &Outcome, actual: &Outcome) -> BTreeSet<Kind> {
    let mut kinds = BTreeSet::new();
    let neither_deadlocks = !expected.deadlock && !actual.deadlock;
    if actual.deadlock && !expected.deadlock {
        kinds.insert(Kind::FalseDeadlock);
    }
    if expected.deadlock && !actual.deadlock {
        kinds.insert(Kind::MissedDeadlock);
    }
    if neither_deadlocks && expected.try_results != actual.try_results {
        kinds.insert(Kind::TryResults);
    }
    if !actual.values.is_subset(&expected.values) {
        kinds.insert(Kind::ShuttleOnlyValues);
    }
    if neither_deadlocks && !expected.values.is_subset(&actual.values) {
        kinds.insert(Kind::MissedValues);
    }
    if actual.overlap && !expected.overlap {
        kinds.insert(Kind::Overlap);
    }
    kinds
}

/// Runs every scenario on both sides and checks the differences against `known`.
///
/// A scenario fails if its differences are not exactly the ones that `known` lists for it: a new
/// difference, a known difference that no longer happens, or a change in the kinds. Prints the full
/// table (visible with `--nocapture`), and on failure also puts it in the panic message.
pub fn check_table(headers: &[&str], scenarios: Vec<Scenario>, known: &[KnownDivergence]) {
    let mut rows = Vec::new();
    let mut errors = Vec::new();

    for scenario in scenarios {
        let expected = explore(&scenario.programs);
        let actual = run_shuttle(&scenario.programs);
        let name = scenario.columns.join(" | ");
        let found = divergences(&expected, &actual);
        let listed: BTreeSet<Kind> = known
            .iter()
            .find(|k| k.scenario == name)
            .map(|k| k.kinds.iter().copied().collect())
            .unwrap_or_default();

        let status = if found == listed {
            if found.is_empty() {
                "match".to_string()
            } else {
                format!("known: {found:?}")
            }
        } else {
            errors.push(format!(
                "{name}: Shuttle differs by {found:?}, but KNOWN_DIVERGENCES lists {listed:?}"
            ));
            format!("UNEXPECTED: {found:?}, listed {listed:?}")
        };

        let mut row = scenario.columns;
        row.extend([describe_outcome(&expected), describe_outcome(&actual), status]);
        rows.push(row);
    }

    let mut all_headers: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
    all_headers.extend(["parking_lot".into(), "shuttle".into(), "status".into()]);
    let widths: Vec<usize> = (0..all_headers.len())
        .map(|i| rows.iter().chain([&all_headers]).map(|r| r[i].len()).max().unwrap())
        .collect();
    let mut table = String::new();
    for row in [&all_headers].into_iter().chain(&rows) {
        let cells: Vec<String> = row.iter().zip(&widths).map(|(c, w)| format!("{c:<w$}")).collect();
        writeln!(table, "{}", cells.join(" | ").trim_end()).unwrap();
    }

    println!("{table}");
    assert!(errors.is_empty(), "\n{table}\n{}", errors.join("\n"));
}

/// Runs the scenario under a DFS scheduler, as `check_dfs` does, and returns what Shuttle does over
/// all schedules.
///
/// When Shuttle finds a deadlock it stops, so `try_results` and `values` then hold only the schedules
/// that finished before the deadlock. The runner does not print the failing schedule of a deadlock,
/// because each deadlock is a result here, not a failure. Shuttle and the default panic hook still
/// print some lines to stderr for each deadlock, which `--nocapture` shows.
pub fn run_shuttle(programs: &[Vec<Op>]) -> Outcome {
    let try_results = Arc::new(StdMutex::new(BTreeSet::new()));
    let values = Arc::new(StdMutex::new(BTreeSet::new()));
    let overlap = Arc::new(AtomicBool::new(false));
    let programs: Arc<[Vec<Op>]> = programs.into();

    let result = {
        let try_results = Arc::clone(&try_results);
        let values = Arc::clone(&values);
        let overlap = Arc::clone(&overlap);
        panic::catch_unwind(AssertUnwindSafe(move || {
            let mut config = Config::new();
            config.failure_persistence = FailurePersistence::None;
            Runner::new(DfsScheduler::new(None, false), config).run(move || {
                let lock = Arc::new(RwLock::new(0u8));
                let holders = Arc::new(Holders {
                    holding: StdMutex::new(vec![Holding::Nothing; programs.len()]),
                    overlap: Arc::clone(&overlap),
                });
                let (tx, rx) = mpsc::channel();
                let mut main = Task {
                    actor: Actor::new(&*lock),
                    index: 0,
                    holders: &holders,
                    tx: None,
                    rx: Some(&rx),
                };

                // Take the lock before the other tasks exist.
                main.run(&programs[0][..1]);
                let handles = (1..programs.len())
                    .map(|index| {
                        let (lock, holders, programs) =
                            (Arc::clone(&lock), Arc::clone(&holders), Arc::clone(&programs));
                        let tx = tx.clone();
                        thread::spawn(move || {
                            let mut task = Task {
                                actor: Actor::new(&*lock),
                                index,
                                holders: &holders,
                                tx: Some(&tx),
                                rx: None,
                            };
                            task.run(&programs[index]);
                            task.results()
                        })
                    })
                    .collect::<Vec<_>>();
                drop(tx);
                main.run(&programs[0][1..]);

                let mut results = vec![main.results()];
                drop(main);
                results.extend(handles.into_iter().map(|h| h.join().unwrap()));
                let mut recorded = Vec::new();
                let mut try_results = try_results.lock().unwrap();
                for (task, (task_values, tries)) in results.into_iter().enumerate() {
                    recorded.push(task_values);
                    try_results.extend(tries.into_iter().map(|(op, ok)| (task, op, ok)));
                }
                values.lock().unwrap().insert(recorded);
            });
        }))
    };

    let deadlock = match result {
        Ok(()) => false,
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or_default();
            if !message.contains(DEADLOCK_PANIC) {
                panic::resume_unwind(payload);
            }
            true
        }
    };
    let try_results = try_results.lock().unwrap().clone();
    let values = values.lock().unwrap().clone();
    Outcome {
        deadlock,
        try_results,
        values,
        overlap: overlap.load(Ordering::SeqCst),
    }
}

/// What each task of one Shuttle execution holds, to find two tasks that hold the lock at the same
/// time in modes that exclude each other.
///
/// A task is in a critical section from the moment a lock op returns until it starts to give up
/// the lock. So it marks the mode that it takes after the op that takes it, and the mode that it
/// keeps before an op that gives up some of the lock (an unlock or a downgrade). No yield point comes
/// between an op and its mark, so at each yield point the marks show the critical sections of all
/// tasks. Shuttle grants a request only when the requesting task runs, which is at a yield point of
/// each other task. So if Shuttle grants a request that the mode of a critical section excludes,
/// `update` finds the overlap. A task that does not yield in its critical section, for example
/// `read` then `unlock`, cannot overlap with another task there, because no other task runs in it.
struct Holders {
    holding: StdMutex<Vec<Holding>>,
    /// Set when some schedule has an overlap. Shared by all the executions of the scenario.
    overlap: Arc<AtomicBool>,
}

impl Holders {
    fn update(&self, index: usize, now: Holding) {
        let compatible = |a, b| {
            matches!(
                (a, b),
                (Holding::Nothing, _)
                    | (_, Holding::Nothing)
                    | (Holding::Shared, Holding::Shared | Holding::Upgradable)
                    | (Holding::Upgradable, Holding::Shared)
            )
        };
        let mut holding = self.holding.lock().unwrap();
        holding[index] = now;
        if (holding.iter().enumerate()).any(|(j, &other)| j != index && !compatible(now, other)) {
            self.overlap.store(true, Ordering::SeqCst);
        }
    }
}

/// One task of a Shuttle execution. Only the task's own end of the channel is given, for its `Signal`
/// or `AwaitSignal` ops.
struct Task<'a> {
    actor: Actor<'a, RawRwLock>,
    index: usize,
    holders: &'a Holders,
    tx: Option<&'a Sender<()>>,
    rx: Option<&'a Receiver<()>>,
}

impl Task<'_> {
    fn run(&mut self, ops: &[Op]) {
        for &op in ops {
            // The mode that an op keeps, if it gives up some of the lock.
            let kept = match op {
                Unlock | UnlockFair => Some(Holding::Nothing),
                Downgrade | DowngradeUpgradable => Some(Holding::Shared),
                DowngradeToUpgradable => Some(Holding::Upgradable),
                _ => None,
            };
            if let Some(kept) = kept {
                self.holders.update(self.index, kept);
            }
            match op {
                Signal => self.tx.expect("this task has no sender").send(()).unwrap(),
                AwaitSignal => self.rx.expect("this task has no receiver").recv().unwrap(),
                _ => {}
            }
            self.actor.step(op);
            self.holders.update(self.index, self.actor.holding());
        }
    }

    /// The task's values and `try_*` results.
    fn results(&mut self) -> (Vec<u8>, Vec<(usize, bool)>) {
        let actor = &mut self.actor;
        (std::mem::take(&mut actor.values), std::mem::take(&mut actor.tries))
    }
}
