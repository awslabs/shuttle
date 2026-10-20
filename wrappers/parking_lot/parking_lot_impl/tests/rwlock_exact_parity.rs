//! Exact parity: does Shuttle's `RwLock` do what `parking_lot` 0.12.5 does, including what its park
//! queue makes observable?
//!
//! `rwlock_admission_parity` and `rwlock_transition_parity` compare Shuttle with the reference
//! explorer, which has no queue (see `rwlock_parity::reference`). It cannot show that a request
//! that an unlock does not wake stays parked, which makes some programs deadlock in `parking_lot`
//! and some `try_write`s fail on a free lock. This crate compares Shuttle with the exact model of
//! `rwlock_parity::exact`, which has the queue, on:
//!
//! * the 600 scenarios of the two tables ([`exact_parity_on_the_admission_table`] and
//!   [`exact_parity_on_the_transition_table`]), where the test also checks that the model has the
//!   outcomes of the reference explorer, as the explorer's docs say;
//! * the queue scenarios of `rwlock_parity::programs`, in which requests park behind the lock
//!   ([`exact_parity_on_queue_scenarios`]);
//! * random programs from the seed `RWLOCK_EXACT_PARITY_SEED`: of three tasks
//!   ([`exact_parity_on_random_programs_of_three_tasks`]), 30 or as many as
//!   `RWLOCK_EXACT_PARITY_PROGRAMS` says, and of four tasks
//!   ([`exact_parity_on_random_programs_of_four_tasks`]), as many as
//!   `RWLOCK_EXACT_PARITY_FOUR_TASK_PROGRAMS` says, by default none. Shuttle takes up to a minute
//!   to explore every schedule of a program of four tasks, so only the weekly `beta` job in
//!   `.github/workflows/crons.yml` runs them, and more programs of three tasks.
//!
//! The model explores every schedule of a program, and the harness runs every schedule of it under
//! Shuttle's DFS scheduler (see `rwlock_parity::harness`), several programs at a time. Run with
//! `--nocapture` to see how many programs deadlock, and in how many the fairness bound leaves
//! outcomes out (see below).
//!
//! # What must match
//!
//! Shuttle must never do what the model cannot do. On every program, the test fails if Shuttle
//! deadlocks where the model does not, records values or `try_*` results that the model does not
//! record, or lets two tasks hold the lock in modes that exclude each other.
//!
//! Shuttle must also do all that the model does: deadlock where the model can, and record all of
//! its values and `try_*` results (compared only when neither deadlocks, because Shuttle stops at
//! its first deadlock). Shuttle's lock leaves some schedules out on purpose, though: in place of
//! `parking_lot`'s timer, its fairness bound lets a woken request in once other requests have
//! overtaken it `Config::parking_lot_fairness_bound` times (2 by default; see
//! `Fairness::ParkingLot`). The model counts overtakes as the engine does, so it knows whether the
//! bound can matter in a program: only if, in some schedule, a woken request has that many
//! overtakes right after a release (see `rwlock_parity::exact`). Where it cannot, Shuttle at the
//! default bound must reach every outcome of the model. Where it can, and Shuttle leaves an outcome out,
//! the test runs Shuttle again without a bound (`usize::MAX`), and fails unless Shuttle then
//! matches the model exactly. The test prints how many programs that happens in, and the queue
//! scenarios pin it where it is known to happen: Shuttle finds the stranded reader only with a
//! larger bound once the woken writer is passed twice.
//!
//! A divergence is a bug in Shuttle's lock or in the model. The failure message gives the program
//! and both sides' outcomes.

// Each test crate uses only part of the shared module.
#[allow(dead_code)]
mod rwlock_parity;

use rwlock_parity::exact::{Exploration, explore_exact};
use rwlock_parity::harness::{QUEUED, REQUESTED, TRANSITIONS, describe, run_shuttle, run_shuttle_with};
use rwlock_parity::programs::{
    AtDefaultBound, four_task_program_count, queue_scenarios, random_programs_of, random_seed, three_task_program_count,
};
use rwlock_parity::reference::Op::{self, *};
use rwlock_parity::reference::{Outcome, explore};
use shuttle::Config;
use std::collections::BTreeMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

/// How Shuttle's outcome differs from the model's.
#[derive(Debug, Default)]
struct Differences {
    /// What Shuttle does and the model cannot.
    unsound: Vec<&'static str>,
    /// What the model does and Shuttle leaves out.
    missed: Vec<&'static str>,
}

fn differences(model: &Outcome, shuttle: &Outcome) -> Differences {
    let mut d = Differences::default();
    if shuttle.deadlock && !model.deadlock {
        d.unsound.push("a deadlock");
    }
    if !shuttle.values.is_subset(&model.values) {
        d.unsound.push("values");
    }
    if !shuttle.try_results.is_subset(&model.try_results) {
        d.unsound.push("try results");
    }
    if shuttle.overlap {
        d.unsound.push("two tasks in the lock in modes that exclude each other");
    }
    if model.deadlock && !shuttle.deadlock {
        d.missed.push("the deadlock");
    }
    // Shuttle stops at its first deadlock, so it records only part of the values and try results
    // then.
    if !model.deadlock && !shuttle.deadlock {
        if !model.values.is_subset(&shuttle.values) {
            d.missed.push("values");
        }
        if !model.try_results.is_subset(&shuttle.try_results) {
            d.missed.push("try results");
        }
    }
    d
}

/// What Shuttle reaches of a program's outcomes in the model, if it does not diverge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// Every outcome, at the default bound.
    Exact,
    /// Not every outcome at the default bound, where the bound can matter. Every outcome without a
    /// bound.
    PrunedByTheBound,
}

/// A program, what the model finds, and Shuttle's verdict or why Shuttle diverges.
struct Checked {
    name: String,
    model: Exploration,
    verdict: Result<Verdict, String>,
}

fn summary(o: &Outcome) -> String {
    let mut tries = BTreeMap::<_, Vec<bool>>::new();
    for &(task, op, ok) in &o.try_results {
        tries.entry((task, op)).or_default().push(ok);
    }
    let tries: Vec<String> = (tries.into_iter())
        .map(|((task, op), results)| format!("try {task}.{op} {results:?}"))
        .collect();
    format!(
        "{}{}, {} value sets {:?}",
        if o.deadlock { "deadlock" } else { "no deadlock" },
        tries.iter().map(|t| format!(", {t}")).collect::<String>(),
        o.values.len(),
        o.values
    )
}

fn describe_programs(programs: &[Vec<Op>]) -> String {
    programs.iter().map(|p| describe(p)).collect::<Vec<_>>().join(" / ")
}

/// Compares Shuttle with the exact model on `programs` (see the module docs).
fn check(name: String, programs: &[Vec<Op>]) -> Checked {
    let model = explore_exact(programs);
    let shuttle = run_shuttle(programs);
    let found = differences(&model.outcome, &shuttle);
    let bound = Config::new().parking_lot_fairness_bound;
    let verdict = if !found.unsound.is_empty() {
        Err(format!("Shuttle gives {:?} that the model does not", found.unsound))
    } else if found.missed.is_empty() {
        Ok(Verdict::Exact)
    } else if model.max_overtakes < bound {
        Err(format!(
            "Shuttle misses {:?} of the model, and the fairness bound ({bound}) cannot explain \
             that: no woken request has more than {} overtakes",
            found.missed, model.max_overtakes
        ))
    } else {
        let mut config = Config::new();
        config.parking_lot_fairness_bound = usize::MAX;
        let unbounded = run_shuttle_with(programs, config);
        let without_bound = differences(&model.outcome, &unbounded);
        if without_bound.unsound.is_empty() && without_bound.missed.is_empty() {
            Ok(Verdict::PrunedByTheBound)
        } else {
            Err(format!(
                "at the default bound, Shuttle misses {:?} of the model, and without a bound it \
                 still differs by {without_bound:?}; without a bound, Shuttle: {}",
                found.missed,
                summary(&unbounded)
            ))
        }
    };
    let verdict = verdict.map_err(|why| {
        let only_in = |a: &Outcome, b: &Outcome| a.values.difference(&b.values).cloned().collect::<Vec<_>>();
        format!(
            "{name} [{}]: {why}\n  value sets only the model has: {:?}\n  value sets only Shuttle has: \
             {:?}\n  model:   {}\n  shuttle: {}",
            describe_programs(programs),
            only_in(&model.outcome, &shuttle),
            only_in(&shuttle, &model.outcome),
            summary(&model.outcome),
            summary(&shuttle)
        )
    });
    Checked { name, model, verdict }
}

/// Checks each program, several at a time, in the order given.
fn check_all(programs: Vec<(String, Vec<Vec<Op>>)>) -> Vec<Checked> {
    let workers = thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(programs.len());
    let next = AtomicUsize::new(0);
    let checked = Mutex::new(Vec::new());
    thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((name, programs)) = programs.get(i) else {
                        break;
                    };
                    let result = check(name.clone(), programs);
                    checked.lock().unwrap().push((i, result));
                }
            });
        }
    });
    let mut checked = checked.into_inner().unwrap();
    checked.sort_by_key(|&(i, _)| i);
    checked.into_iter().map(|(_, c)| c).collect()
}

/// Prints how the programs did, and fails with every divergence.
fn report(what: &str, checked: &[Checked]) {
    let bound = Config::new().parking_lot_fairness_bound;
    let count = |f: &dyn Fn(&Checked) -> bool| checked.iter().filter(|c| f(c)).count();
    let pruned: Vec<&str> = (checked.iter())
        .filter(|c| c.verdict == Ok(Verdict::PrunedByTheBound))
        .map(|c| c.name.as_str())
        .collect();
    let errors: Vec<&str> = (checked.iter())
        .filter_map(|c| c.verdict.as_ref().err())
        .map(String::as_str)
        .collect();
    println!(
        "{what}: {} programs, {} of which deadlock in the model. Shuttle matches the model in {}. \
         The fairness bound ({bound}) can matter in {}, and leaves out outcomes of the model in {} \
         ({pruned:?}), which Shuttle reaches without a bound. {} diverge.",
        checked.len(),
        count(&|c| c.model.outcome.deadlock),
        count(&|c| c.verdict == Ok(Verdict::Exact)),
        count(&|c| c.model.max_overtakes >= bound),
        pruned.len(),
        errors.len()
    );
    assert!(
        errors.is_empty(),
        "Shuttle's RwLock diverges from the exact model of parking_lot:\n{}",
        errors.join("\n")
    );
}

/// The admission table of `rwlock_admission_parity`: the main task holds the lock in each mode
/// while another task waits and a third makes each request.
fn admission_table() -> Vec<(String, Vec<Vec<Op>>)> {
    let mut scenarios = Vec::new();
    for held in [Read, UpgradableRead, Write] {
        for &queued in QUEUED {
            for &requested in REQUESTED {
                let mut programs = vec![vec![held, AwaitSignal, Unlock]];
                if !queued.is_empty() {
                    programs.push([queued, &[Unlock]].concat());
                }
                programs.push([requested, &[Signal, Unlock]].concat());
                let name = format!("{} | {} | {}", held.name(), describe(queued), describe(requested));
                scenarios.push((name, programs));
            }
        }
    }
    scenarios
}

/// The transition table of `rwlock_transition_parity`: the main task upgrades or downgrades its
/// lock, then unlocks (`release`), or holds the lock until the third task's signal (`hold`).
fn transition_table() -> Vec<(String, Vec<Vec<Op>>)> {
    let mut scenarios = Vec::new();
    for &main in TRANSITIONS {
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
                    let name = format!(
                        "{} | {} | {} | {}",
                        describe(main),
                        if hold { "hold" } else { "release" },
                        describe(queued),
                        describe(requested)
                    );
                    scenarios.push((name, programs));
                }
            }
        }
    }
    scenarios
}

/// Fails unless the reference explorer has the outcomes of the exact model on each of the table's
/// scenarios, which the explorer's module docs (`rwlock_parity::reference`) say of the parity
/// tables, and which their comparison of Shuttle with the explorer relies on.
fn check_the_reference_explorer(checked: &[Checked], scenarios: &[(String, Vec<Vec<Op>>)]) {
    let differ: Vec<&str> = (checked.iter().zip(scenarios))
        .filter(|(checked, (_, programs))| explore(programs) != checked.model.outcome)
        .map(|(checked, _)| checked.name.as_str())
        .collect();
    assert!(
        differ.is_empty(),
        "the reference explorer and the exact model differ on {differ:?}"
    );
}

/// The 120 scenarios of the admission table, where the main task holds the lock in each mode while
/// another task's request waits and a third task makes each request.
#[test]
fn exact_parity_on_the_admission_table() {
    let scenarios = admission_table();
    let checked = check_all(scenarios.clone());
    report("admission table", &checked);
    check_the_reference_explorer(&checked, &scenarios);
}

/// The 480 scenarios of the transition table, where the main task's upgrades and downgrades also
/// wake the queue.
#[test]
fn exact_parity_on_the_transition_table() {
    let scenarios = transition_table();
    let checked = check_all(scenarios.clone());
    report("transition table", &checked);
    check_the_reference_explorer(&checked, &scenarios);
}

/// The queue scenarios, in which requests park behind the lock, so that the outcomes can depend on
/// the order in which they park and on which of them a release wakes: the stranded reader,
/// `try_write` against parked requests, woken requests that lose the race to the task that
/// unlocked, and fair unlocks. Besides the parity, this pins whether the model deadlocks, and
/// whether Shuttle reaches every outcome at its default fairness bound (see
/// `rwlock_parity::programs`): a change in either is a change in `parking_lot`'s behavior as the
/// model has it, or in what Shuttle covers.
#[test]
fn exact_parity_on_queue_scenarios() {
    let scenarios = queue_scenarios();
    let checked = check_all(
        scenarios
            .iter()
            .map(|s| (s.name.to_string(), s.programs.clone()))
            .collect(),
    );
    let mut unexpected = Vec::new();
    for (scenario, checked) in scenarios.iter().zip(&checked) {
        if checked.model.outcome.deadlock != scenario.deadlocks {
            unexpected.push(format!(
                "{}: the model {} deadlock, but the scenario says it {}",
                scenario.name,
                if checked.model.outcome.deadlock {
                    "can"
                } else {
                    "cannot"
                },
                if scenario.deadlocks { "can" } else { "cannot" }
            ));
        }
        let expected = match scenario.at_default_bound {
            AtDefaultBound::Every => Verdict::Exact,
            AtDefaultBound::Some => Verdict::PrunedByTheBound,
        };
        if let Ok(verdict) = checked.verdict
            && verdict != expected
        {
            unexpected.push(format!(
                "{}: Shuttle's verdict is {verdict:?}, but the scenario says {expected:?}",
                scenario.name
            ));
        }
    }
    report("queue scenarios", &checked);
    assert!(unexpected.is_empty(), "{}", unexpected.join("\n"));
}

/// Checks the first `count` random programs of `tasks` tasks from the seed
/// `RWLOCK_EXACT_PARITY_SEED` (see `rwlock_parity::programs::random_programs`). Also prints in how
/// many of them the exact model differs from the reference explorer, which has no queue, or the
/// explorer refuses the program because a `try_write` can meet a parked request (see
/// `rwlock_parity::reference`).
fn check_random_programs(tasks: usize, count: usize, count_variable: &str) {
    let what = format!("random programs of {tasks} tasks");
    if count == 0 {
        println!("{what}: none, as {count_variable} says");
        return;
    }
    let seed = random_seed();
    let programs = random_programs_of(tasks, count, seed);
    let checked = check_all(
        (programs.iter())
            .map(|(n, programs)| (format!("random program {n} of seed {seed}"), programs.clone()))
            .collect(),
    );
    let queue_matters = (programs.iter().zip(&checked))
        .filter(|((_, programs), checked)| {
            // The explorer panics on a program that it refuses.
            let reference = panic::catch_unwind(AssertUnwindSafe(|| explore(programs))).ok();
            reference.as_ref() != Some(&checked.model.outcome)
        })
        .count();
    println!(
        "{what}: the exact model differs from the reference explorer, or the explorer refuses the \
         program, in {queue_matters} of {count}"
    );
    report(&what, &checked);
}

/// Random programs of three tasks with every op: `RWLOCK_EXACT_PARITY_PROGRAMS` of them, 30 by
/// default.
#[test]
fn exact_parity_on_random_programs_of_three_tasks() {
    check_random_programs(3, three_task_program_count(), "RWLOCK_EXACT_PARITY_PROGRAMS");
}

/// Random programs of four tasks with every op: `RWLOCK_EXACT_PARITY_FOUR_TASK_PROGRAMS` of them,
/// none by default, because Shuttle takes up to a minute to explore every schedule of one of them.
/// The weekly `beta` job in `.github/workflows/crons.yml` runs 30, which with 120 of three tasks
/// are the first 150 programs of the default seed.
#[test]
fn exact_parity_on_random_programs_of_four_tasks() {
    check_random_programs(4, four_task_program_count(), "RWLOCK_EXACT_PARITY_FOUR_TASK_PROGRAMS");
}
