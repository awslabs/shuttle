//! Checks the reference model of the parity tests (`rwlock_parity::reference`) against the real
//! `parking_lot` crate, on OS threads.
//!
//! The parity tables compare Shuttle with the model, not with `parking_lot`. This test ties the model
//! to `parking_lot`: it fails if a rule of the model is wrong, or if a `parking_lot` release changes
//! the behavior that the model copies. It uses the scenarios of both tables.
//!
//! * [`reference_model_matches_parking_lot_in_fixed_order`] runs each scenario once, in one fixed
//!   order, and compares each state that it reaches with the state that the model's rules (`step`)
//!   give for that order:
//!   1. Thread A takes the lock in its first mode.
//!   2. Thread B runs its requests as far as it can, and keeps what it gets.
//!   3. Thread A runs its transitions, one at a time. After each one, B goes on as far as it can.
//!   4. Thread C runs its requests. Each blocking request is a timed request, so "refused" means
//!      "timed out".
//!
//!   After step 2 and after each transition, the test waits until A and B are in the state that the
//!   model predicts, and fails if they go past it. At the end it compares C's results. So this
//!   checks each rule of the model in each lock state that the scenarios reach in this order. The
//!   explorer tries the other orders with the same rules.
//!
//! * [`reference_model_covers_parking_lot_under_stress`] runs each `release` scenario of the
//!   transition table many times, in the same way as the Shuttle harness, with random delays between
//!   ops. Each scenario runs in two variants: every task's final unlock is plain, or every task's
//!   final unlock is `unlock_fair`, whose hand-off the model also gives the plain unlock rule (see
//!   the reference module docs). Each result of `parking_lot` (the `try_*` results and the recorded
//!   values) must also be a result of the model. This tests that the model does not leave out a
//!   behavior of `parking_lot`, and in particular that it needs neither the order in which
//!   `parking_lot` wakes tasks nor the fair hand-off. Set `RWLOCK_PARITY_STRESS_ITERATIONS` to
//!   change the number of runs of each scenario (default 100, half per variant). Run with
//!   `--nocapture` to see how many of the model's results `parking_lot` gave.
//!
//! * [`parking_lot_lets_a_later_writer_overtake_a_parked_one`] shows that `parking_lot` does not
//!   grant the lock in the order in which writers park: after a plain unlock, a writer that parked
//!   later can get the lock first.
//!
//! # Timing
//!
//! The fixed-order test goes on to the next step only when it observes the state that the model
//! predicts. It waits for that state for up to [`DEADLINE`]:
//! * For each op, the thread counts the op before it starts it and after it returns (see
//!   [`Progress`]). The test waits for the count of each op that the model says returns.
//! * For a `write` or an `upgrade` that waits for readers, the test waits until
//!   `RwLock::is_locked_exclusive` shows `WRITER_BIT`.
//! * For an op that the model says blocks, the test waits until the thread starts the op, pauses for
//!   [`PARK_PAUSE`], and then checks that the op did not return.
//!
//! The pause is the only part that depends on timing, and a stall there cannot make the test fail.
//! If the model is correct, a late thread only parks when it runs, and a park sets only `PARKED_BIT`.
//! No request in the scenarios checks that bit: `read` and `try_read` check only `WRITER_BIT`,
//! `upgradable_read` and `write` check `WRITER_BIT | UPGRADABLE_BIT`, and `try_write` fails in each
//! case, because A holds the lock until C ends. If the model is wrong, a stall there can only hide
//! the mismatch in that run.
//!
//! A and B do not change the lock state while C runs, so C's results depend only on that state. A
//! request that the lock grants returns at once, so C's timeout ([`REFUSE_TIMEOUT`]) only decides
//! how long a refused request waits. So a thread that the OS does not run for some time makes the
//! test slower. It makes the test fail only if it does not run for the whole [`DEADLINE`].
//!
//! The stress test does not depend on timing, because each order that `parking_lot` gives must be an
//! order of the model. Only its watchdog uses a time limit (see [`STUCK`]).
//!
//! The overtaking test waits until each writer asks for the lock, and then pauses so that it parks.
//! A stall there, or a run in which eventual fairness hands the lock to W2, only makes a run show no
//! overtaking, and the test then runs again.

// Each test crate uses only part of the shared module.
#[allow(dead_code)]
mod rwlock_parity;

use parking_lot::{RawRwLock, RwLock, RwLockUpgradableReadGuard};
use rwlock_parity::actor::{self, Actor};
use rwlock_parity::harness::{QUEUED, REQUESTED, TRANSITIONS, describe};
use rwlock_parity::reference::Op::{self, *};
use rwlock_parity::reference::{Holding, Word, explore, step};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

/// The main task's modes in the admission table, which has no transitions.
const HELD: &[Op] = &[Read, UpgradableRead, Write];

/// How long the fixed-order test waits for a state that the model predicts. A run reaches each state
/// in less than a millisecond, so only a wrong rule of the model, or a thread that the OS does not
/// run for this long, makes a wait end at the deadline.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long the fixed-order test waits after a thread starts an op that the model says blocks,
/// before it checks that the op did not return. See the module docs for why a stall here cannot make
/// the test fail.
const PARK_PAUSE: Duration = Duration::from_millis(50);

/// The timeout of C's blocking requests in the fixed-order test. A request that the lock grants
/// returns at once, so this only decides how long a refused request waits.
const REFUSE_TIMEOUT: Duration = Duration::from_millis(50);

/// How often the fixed-order test looks at the state while it waits.
const POLL: Duration = Duration::from_millis(1);

/// The number of scenarios that the fixed-order test runs at the same time. The threads sleep or
/// park for almost all of the time, so this number does not depend on the number of CPUs.
const FIXED_WORKERS: usize = 16;

/// The stress test fails if `parking_lot` makes no progress for this long. That is a deadlock in a
/// scenario where the model says that no schedule deadlocks.
const STUCK: Duration = Duration::from_secs(30);

/// The main task's programs: the admission table's (a first mode and no transitions), then the
/// transition table's.
fn main_programs() -> impl Iterator<Item = &'static [Op]> {
    HELD.iter().map(std::slice::from_ref).chain(TRANSITIONS.iter().copied())
}

fn is_try(op: Op) -> bool {
    matches!(op, TryRead | TryUpgradableRead | TryWrite | TryUpgrade)
}

// ---------------------------------------------------------------------------------------------
// The real lock.

type Guard<'a> = actor::Guard<'a, RawRwLock>;

/// Runs one op, as `Guard::apply` does, except that a blocking request gives up after `timeout`.
/// Returns the new guard and whether the op succeeded. Shuttle does not implement the timed requests,
/// so they are here and not in the shared actor.
fn apply_timed<'a>(lock: &'a RwLock<u8>, guard: Guard<'a>, op: Op, timeout: Duration) -> (Guard<'a>, Option<bool>) {
    match (op, guard) {
        (Read, Guard::Nothing) => Guard::tried(lock.try_read_for(timeout), Guard::Read),
        (UpgradableRead, Guard::Nothing) => Guard::tried(lock.try_upgradable_read_for(timeout), Guard::Upgradable),
        (Write, Guard::Nothing) => Guard::tried(lock.try_write_for(timeout), Guard::Write),
        (Upgrade, Guard::Upgradable(g)) => match RwLockUpgradableReadGuard::try_upgrade_for(g, timeout) {
            Ok(g) => (Guard::Write(g), Some(true)),
            Err(g) => (Guard::Upgradable(g), Some(false)),
        },
        (op, guard) => guard.apply(lock, op),
    }
}

/// The hint in the failure messages of the tests that compare the model with `parking_lot`.
const NEW_RELEASE_HINT: &str = "The model copies parking_lot 0.12.5, but the build can use a later 0.12.x release (see \
                                Cargo.toml). If your change does not touch the model, check if a later release \
                                changed this behavior.";

// ---------------------------------------------------------------------------------------------
// Fixed order.

/// Runs `op` for one task of the model in place. Returns false if the task is blocked (refused, or
/// a `write`/`upgrade` that waits for readers), and the result of a `try_*` op.
fn model_step(word: &mut Word, holding: &mut Holding, op: Op) -> (bool, Option<bool>) {
    match step(*word, *holding, 0, op) {
        None => (false, None),
        Some(s) => {
            *word = s.word;
            *holding = s.holding;
            (s.advance, s.try_result)
        }
    }
}

/// Runs a task of the model from `pc` until it blocks or ends.
fn model_run(word: &mut Word, holding: &mut Holding, pc: &mut usize, ops: &[Op]) {
    while let Some(&op) = ops.get(*pc) {
        if !model_step(word, holding, op).0 {
            return;
        }
        *pc += 1;
    }
}

/// A state of the fixed order: after B starts, and after each of A's transitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Checkpoint {
    /// The number of A's ops that A may start.
    a_started: usize,
    /// The number of A's ops that returned. If this is less than `a_started`, A is blocked in its
    /// next op.
    a_done: usize,
    /// The number of B's ops that returned. B starts each of its ops as soon as it can, so if this
    /// is less than the length of its program, B is blocked in its next op.
    b_done: usize,
    /// Whether a task owns `WRITER_BIT`.
    writer: bool,
}

/// What the model says the fixed order gives.
struct Prediction {
    checkpoints: Vec<Checkpoint>,
    /// The results of C's requests. They stop at the first refused blocking request, as C does.
    results: Vec<bool>,
}

fn predict(main: &[Op], queued: &[Op], requested: &[Op]) -> Prediction {
    let mut word = Word::default();
    let mut a = Holding::Nothing;
    assert!(
        model_step(&mut word, &mut a, main[0]).0,
        "the first request on a free lock"
    );

    let (mut b, mut b_done) = (Holding::Nothing, 0);
    model_run(&mut word, &mut b, &mut b_done, queued);
    let mut checkpoints = vec![Checkpoint {
        a_started: 1,
        a_done: 1,
        b_done,
        writer: word.writer,
    }];
    for (a_started, &op) in (2..).zip(&main[1..]) {
        let (advanced, _) = model_step(&mut word, &mut a, op);
        model_run(&mut word, &mut b, &mut b_done, queued);
        checkpoints.push(Checkpoint {
            a_started,
            a_done: a_started - usize::from(!advanced),
            b_done,
            writer: word.writer,
        });
        if !advanced {
            break;
        }
    }

    let mut c = Holding::Nothing;
    let mut results = Vec::new();
    for &op in requested {
        let (advanced, try_result) = model_step(&mut word, &mut c, op);
        results.push(try_result.unwrap_or(advanced));
        if !advanced && try_result.is_none() {
            break;
        }
    }
    Prediction { checkpoints, results }
}

/// How far a thread of the fixed order has run its ops.
#[derive(Default)]
struct Progress {
    /// The number of ops that the thread started.
    started: AtomicUsize,
    /// The number of ops that returned.
    done: AtomicUsize,
}

impl Progress {
    /// Runs a blocking op, and counts it before it starts and after it returns.
    fn run<'a>(&self, lock: &'a RwLock<u8>, guard: Guard<'a>, op: Op) -> Guard<'a> {
        self.started.fetch_add(1, Ordering::SeqCst);
        let guard = guard.apply(lock, op).0;
        self.done.fetch_add(1, Ordering::SeqCst);
        guard
    }
}

/// What the fixed-order test can see of the real lock and of threads A and B.
#[derive(Debug)]
struct Observed {
    a_started: usize,
    a_done: usize,
    b_started: usize,
    b_done: usize,
    writer: bool,
}

/// The threads A and B of one run of the fixed order.
struct Run {
    lock: Arc<RwLock<u8>>,
    a: Arc<Progress>,
    b: Arc<Progress>,
    /// Lets A start its next op.
    go: Option<mpsc::Sender<()>>,
    /// Tell A and B to drop what they hold and end.
    release: Vec<mpsc::Sender<()>>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl Run {
    /// Starts A. A runs `main[0]` at once, and each other op of `main` when main lets it (see
    /// `go`).
    fn start(main: &'static [Op]) -> Self {
        let lock = Arc::new(RwLock::new(0u8));
        let a = Arc::new(Progress::default());
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let thread = {
            let (lock, a) = (Arc::clone(&lock), Arc::clone(&a));
            thread::spawn(move || {
                let mut guard = a.run(&lock, Guard::Nothing, main[0]);
                for &op in &main[1..] {
                    // Main drops `go` when the model says that A does not run more ops.
                    if go_rx.recv().is_err() {
                        break;
                    }
                    guard = a.run(&lock, guard, op);
                }
                let _ = release_rx.recv();
                drop(guard);
            })
        };
        Self {
            lock,
            a,
            b: Arc::default(),
            go: Some(go_tx),
            release: vec![release_tx],
            threads: vec![thread],
        }
    }

    /// Starts B, which runs all of `queued` as soon as it can.
    fn start_b(&mut self, queued: &'static [Op]) {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (lock, b) = (Arc::clone(&self.lock), Arc::clone(&self.b));
        self.threads.push(thread::spawn(move || {
            let mut guard = Guard::Nothing;
            for &op in queued {
                guard = b.run(&lock, guard, op);
            }
            let _ = release_rx.recv();
            drop(guard);
        }));
        self.release.push(release_tx);
    }

    fn observe(&self) -> Observed {
        Observed {
            a_started: self.a.started.load(Ordering::SeqCst),
            a_done: self.a.done.load(Ordering::SeqCst),
            b_started: self.b.started.load(Ordering::SeqCst),
            b_done: self.b.done.load(Ordering::SeqCst),
            writer: self.lock.is_locked_exclusive(),
        }
    }

    /// Waits, up to `DEADLINE`, until `ready` is true of the observed state. Returns the last
    /// observed state, or an error that shows it.
    fn wait(&self, ready: impl Fn(&Observed) -> bool) -> Result<Observed, String> {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let observed = self.observe();
            if ready(&observed) {
                return Ok(observed);
            }
            if Instant::now() >= deadline {
                return Err(format!("not reached in {DEADLINE:?}, parking_lot {observed:?}"));
            }
            thread::sleep(POLL);
        }
    }

    /// Waits until A and B reach `expected` (see the module docs), and checks that they do not go
    /// past it. `b_len` is the length of B's program.
    fn reach(&self, expected: Checkpoint, b_len: usize) -> Result<(), String> {
        let a_blocked = expected.a_done < expected.a_started;
        let b_blocked = expected.b_done < b_len;
        let mut observed = self
            .wait(|o| {
                o.a_done >= expected.a_done
                    && o.b_done >= expected.b_done
                    && (!a_blocked || o.a_started > expected.a_done)
                    && (!b_blocked || o.b_started > expected.b_done)
                    && (!expected.writer || o.writer)
            })
            .map_err(|why| format!("model {expected:?}: {why}"))?;
        if a_blocked || b_blocked {
            thread::sleep(PARK_PAUSE);
            observed = self.observe();
        }
        if (observed.a_done, observed.b_done, observed.writer) != (expected.a_done, expected.b_done, expected.writer) {
            return Err(format!("model {expected:?}, parking_lot {observed:?}"));
        }
        Ok(())
    }

    /// Runs the scenario after A has started, and compares each state and C's results with the
    /// model.
    fn drive(
        &mut self,
        main: &'static [Op],
        queued: &'static [Op],
        requested: &'static [Op],
        expected: &Prediction,
    ) -> Result<(), String> {
        self.wait(|o| o.a_done >= 1)
            .map_err(|why| format!("A's first op: {why}"))?;
        self.start_b(queued);
        for (i, &checkpoint) in expected.checkpoints.iter().enumerate() {
            let step = if i == 0 {
                "after B starts".to_string()
            } else {
                self.go.as_ref().unwrap().send(()).unwrap();
                format!("after A's {}", main[i].name())
            };
            self.reach(checkpoint, queued.len())
                .map_err(|why| format!("{step}: {why}"))?;
        }
        self.go = None;

        let lock = Arc::clone(&self.lock);
        let results = thread::spawn(move || {
            let mut guard = Guard::Nothing;
            let mut results = Vec::new();
            for &op in requested {
                let (g, ok) = apply_timed(&lock, guard, op, REFUSE_TIMEOUT);
                guard = g;
                let ok = ok.unwrap_or(true);
                results.push(ok);
                if !ok && !is_try(op) {
                    break;
                }
            }
            results
        })
        .join()
        .unwrap();
        if results != expected.results {
            return Err(format!(
                "C's results: model {:?}, parking_lot {results:?}",
                expected.results
            ));
        }
        Ok(())
    }

    /// Releases A and B, and waits for them to end. A waits for `go` or for its release, and B for
    /// its release, so each of them ends when the other drops what it holds.
    fn finish(mut self) {
        self.go = None;
        for release in &self.release {
            let _ = release.send(());
        }
        for thread in self.threads {
            thread.join().unwrap();
        }
    }
}

/// Runs one scenario in the fixed order. Returns why it does not match the model.
fn check_fixed(main: &'static [Op], queued: &'static [Op], requested: &'static [Op]) -> Result<(), String> {
    let expected = predict(main, queued, requested);
    let mut run = Run::start(main);
    let result = run.drive(main, queued, requested, &expected);
    run.finish();
    result
}

#[test]
fn reference_model_matches_parking_lot_in_fixed_order() {
    let scenarios: Vec<(&[Op], &[Op], &[Op])> = main_programs()
        .flat_map(|main| {
            QUEUED
                .iter()
                .flat_map(move |&queued| REQUESTED.iter().map(move |&requested| (main, queued, requested)))
        })
        .collect();

    // `FIXED_WORKERS` scenarios at a time.
    let next = AtomicUsize::new(0);
    let mismatches = Mutex::new(Vec::new());
    thread::scope(|s| {
        for _ in 0..FIXED_WORKERS {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&(main, queued, requested)) = scenarios.get(i) else {
                        break;
                    };
                    if let Err(why) = check_fixed(main, queued, requested) {
                        let name = format!("{} | {} | {}", describe(main), describe(queued), describe(requested));
                        mismatches.lock().unwrap().push((i, format!("{name}: {why}")));
                    }
                }
            });
        }
    });
    let mut mismatches = mismatches.into_inner().unwrap();
    mismatches.sort();
    let mismatches: Vec<_> = mismatches.into_iter().map(|(_, m)| m).collect();

    println!(
        "fixed order: {} scenarios, {} mismatches",
        scenarios.len(),
        mismatches.len()
    );
    assert!(
        mismatches.is_empty(),
        "the reference model differs from parking_lot:\n{}\n{NEW_RELEASE_HINT}",
        mismatches.join("\n")
    );
}

// ---------------------------------------------------------------------------------------------
// Stress.

/// A small xorshift generator, so that the test needs no other crate.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A random delay: often none, sometimes a yield, a short spin, or a short sleep.
    fn delay(&mut self) {
        match self.next() % 8 {
            0..=3 => {}
            4 | 5 => thread::yield_now(),
            6 => {
                for _ in 0..self.next() % 200 {
                    std::hint::spin_loop();
                }
            }
            _ => thread::sleep(Duration::from_micros(1 + self.next() % 50)),
        }
    }
}

/// The values and the `try_*` results of one run: each task's values, and (task, op index, result)
/// for each `try_*` op, as in `Outcome`.
type RunResult = (Vec<Vec<u8>>, Vec<(usize, usize, bool)>);

/// Runs a task's ops with random delays, like one task of the Shuttle harness.
fn run_task(lock: &RwLock<u8>, ops: &[Op], rng: &mut Rng) -> (Vec<u8>, Vec<(usize, bool)>) {
    let mut actor = Actor::new(lock);
    for &op in ops {
        rng.delay();
        actor.step(op);
    }
    (std::mem::take(&mut actor.values), std::mem::take(&mut actor.tries))
}

/// One run of the scenario, in the same order as the Shuttle harness: the main task runs its first
/// op, spawns the others, runs the rest, and joins them.
fn real_once(programs: &Arc<Vec<Vec<Op>>>, seed: u64) -> RunResult {
    let lock = Arc::new(RwLock::new(0u8));
    let mut rng = Rng(seed | 1);
    let mut main = Actor::new(&*lock);
    main.step(programs[0][0]);

    let handles: Vec<_> = (1..programs.len())
        .map(|i| {
            let lock = Arc::clone(&lock);
            let programs = Arc::clone(programs);
            let seed = seed.wrapping_mul(31).wrapping_add(i as u64) | 1;
            thread::spawn(move || run_task(&lock, &programs[i], &mut Rng(seed)))
        })
        .collect();
    for &op in &programs[0][1..] {
        rng.delay();
        main.step(op);
    }

    let mut results = vec![(std::mem::take(&mut main.values), std::mem::take(&mut main.tries))];
    drop(main);
    results.extend(handles.into_iter().map(|h| h.join().unwrap()));
    let (mut values, mut tries) = (Vec::new(), Vec::new());
    for (task, (task_values, task_tries)) in results.into_iter().enumerate() {
        values.push(task_values);
        tries.extend(task_tries.into_iter().map(|(op, ok)| (task, op, ok)));
    }
    (values, tries)
}

/// Runs each `release` scenario of the transition table `iterations` times. Adds 1 to `progress`
/// for each run, and keeps the scenario's name in `current`. Returns the mismatches.
fn stress(iterations: u64, progress: &AtomicU64, current: &Mutex<String>) -> Vec<String> {
    let started = Instant::now();
    let (mut checked, mut skipped, mut mismatches) = (0u64, 0, Vec::new());
    let (mut model_total, mut model_seen) = (0, 0);
    for &main in TRANSITIONS {
        for &queued in QUEUED {
            for &requested in REQUESTED {
                for unlock in [Unlock, UnlockFair] {
                    let name = format!(
                        "{} | release ({}) | {} | {}",
                        describe(main),
                        unlock.name(),
                        describe(queued),
                        describe(requested)
                    );
                    let mut programs = vec![[main, &[unlock]].concat()];
                    if !queued.is_empty() {
                        programs.push([queued, &[unlock]].concat());
                    }
                    programs.push([requested, &[unlock]].concat());
                    let expected = explore(&programs);
                    if expected.deadlock {
                        skipped += 1;
                        continue;
                    }
                    *current.lock().unwrap() = name.clone();

                    let programs = Arc::new(programs);
                    let mut values = BTreeSet::new();
                    let mut tries = BTreeSet::new();
                    for i in 0..iterations.div_ceil(2) {
                        let (v, t) = real_once(&programs, i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ checked);
                        values.insert(v);
                        tries.extend(t);
                        progress.fetch_add(1, Ordering::Relaxed);
                    }

                    checked += 1;
                    model_total += expected.values.len();
                    model_seen += values.intersection(&expected.values).count();
                    let extra_values: Vec<_> = values.difference(&expected.values).collect();
                    let extra_tries: Vec<_> = tries.difference(&expected.try_results).collect();
                    if !extra_values.is_empty() || !extra_tries.is_empty() {
                        mismatches.push(format!(
                            "{name}: parking_lot gave values {extra_values:?} and try results \
                         {extra_tries:?} that the model does not"
                        ));
                    }
                }
            }
        }
    }
    println!(
        "stress: {checked} scenarios x {} runs in {:.1?}, {skipped} skipped (the model \
         deadlocks), {} mismatches; parking_lot gave {model_seen} of the model's {model_total} value \
         sets",
        iterations.div_ceil(2),
        started.elapsed(),
        mismatches.len()
    );
    mismatches
}

#[test]
fn reference_model_covers_parking_lot_under_stress() {
    let iterations = std::env::var("RWLOCK_PARITY_STRESS_ITERATIONS").map_or(100, |n| {
        n.parse().expect("RWLOCK_PARITY_STRESS_ITERATIONS must be a number")
    });
    let progress = Arc::new(AtomicU64::new(0));
    let current = Arc::new(Mutex::new(String::new()));
    let worker = {
        let (progress, current) = (Arc::clone(&progress), Arc::clone(&current));
        thread::spawn(move || stress(iterations, &progress, &current))
    };

    // A watchdog. A run that deadlocks never ends, so the test fails here instead of hanging.
    let (mut seen, mut since) = (0, Instant::now());
    while !worker.is_finished() {
        thread::sleep(Duration::from_millis(100));
        let now = progress.load(Ordering::Relaxed);
        if now != seen {
            (seen, since) = (now, Instant::now());
        } else if since.elapsed() > STUCK {
            panic!(
                "parking_lot made no progress for {STUCK:?} in `{}`, where the model says that no \
                 schedule deadlocks.\n{NEW_RELEASE_HINT}",
                current.lock().unwrap()
            );
        }
    }
    let mismatches = worker.join().unwrap();
    assert!(
        mismatches.is_empty(),
        "parking_lot gave results that the reference model does not:\n{}\n{NEW_RELEASE_HINT}",
        mismatches.join("\n")
    );
}

/// The model's guard for `PARKED_BIT` (see the reference module docs): two writers queue behind the
/// main task's `write`, so when the main task unlocks, both can still be parked while a `try_write`
/// finds the lock free. `parking_lot` can then fail the `try_write`, and the model cannot give that
/// result, so it must refuse the program.
#[test]
#[should_panic(expected = "outside the model")]
fn reference_model_refuses_try_write_behind_two_parked_writers() {
    let writer = vec![Write, Unlock];
    explore(&[writer.clone(), writer.clone(), writer, vec![TryWrite, Unlock]]);
}

/// `parking_lot` does not grant the lock in the order in which writers ask for it, which is why the
/// model lets any waiting request win after a plain unlock. (The Shuttle lock hands the lock over on
/// every unlock instead, see the `raw_rwlock` module docs and #259.) A plain unlock wakes the first
/// parked writer but leaves the lock free, so a thread that is not parked can take it first
/// (`lock_exclusive_slow` grabs `WRITER_BIT` "even if there are parked threads"). The woken writer
/// then finds the lock taken and parks again, behind the writers that parked after it.
///
/// W1, W2 and W3 ask for the lock in that order while the main thread holds it, 20 ms apart, so
/// they park in that order. The main thread held the lock for more than a millisecond, so eventual
/// fairness makes its unlock a hand-off to W1, and starts a new timer of 0 to 1 ms. W1 unlocks at
/// once, so unless that timer has already run out, this is a plain unlock: it wakes W2, and W1
/// locks again before W2 runs. W2 parks again, behind W3, and W1's next unlock hands the lock to
/// W3. In a run where the timer has run out, W1's first unlock hands the lock to W2 instead, so the
/// test runs until W3 overtakes W2.
#[test]
fn parking_lot_lets_a_later_writer_overtake_a_parked_one() {
    /// Long enough for a writer that asks for the lock to park.
    const PARK: Duration = Duration::from_millis(20);
    const RUNS: usize = 50;

    let mut orders = Vec::new();
    for _ in 0..RUNS {
        let lock = Arc::new(RwLock::new(Vec::new()));
        let held = lock.write();
        let writer = |name: &'static str, relock: bool| {
            let lock = Arc::clone(&lock);
            let (asking, asked) = mpsc::channel();
            let handle = thread::spawn(move || {
                asking.send(()).unwrap();
                lock.write().push(name);
                if relock {
                    let mut guard = lock.write();
                    guard.push(name);
                    // Gives the woken W2 the time to find the lock taken and park again.
                    thread::sleep(PARK);
                }
            });
            asked.recv().unwrap();
            thread::sleep(PARK);
            handle
        };
        let writers = [writer("W1", true), writer("W2", false), writer("W3", false)];
        drop(held);
        for handle in writers {
            handle.join().unwrap();
        }
        let order = Arc::into_inner(lock).unwrap().into_inner();
        let turn = |name| order.iter().position(|&n| n == name).unwrap();
        if turn("W3") < turn("W2") {
            return;
        }
        orders.push(order);
    }
    panic!("W2 got the lock before W3 in all {RUNS} runs: {orders:?}");
}
