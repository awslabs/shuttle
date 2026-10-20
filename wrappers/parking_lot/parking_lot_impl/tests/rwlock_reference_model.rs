//! Checks the models of the parity tests against the real `parking_lot` crate, on OS threads: the
//! reference model (`rwlock_parity::reference`), the exact model with its park queue
//! (`rwlock_parity::exact`), and the behavior of `parking_lot` that the models and Shuttle's locks
//! copy.
//!
//! The parity tests compare Shuttle with the models, not with `parking_lot`. These tests tie the
//! models to `parking_lot`: they fail if a rule of a model is wrong, or if a `parking_lot` release
//! changes the behavior that the models copy. For the reference model, they use the scenarios of
//! both tables:
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
//!   transition table many times, in the same way as the Shuttle harness, with random delays
//!   between ops. Each scenario runs in two variants: every task's final unlock is plain, or every
//!   task's final unlock is `unlock_fair`, whose hand-off the model also gives the plain unlock
//!   rule (see the reference module docs). Each result of `parking_lot` (the `try_*` results and
//!   the recorded values) must also be a result of the model. This tests that the model does not
//!   leave out a behavior of `parking_lot` in these scenarios, and in particular that there it
//!   needs neither the order in which `parking_lot` wakes tasks nor the fair hand-off. Set
//!   `RWLOCK_PARITY_STRESS_ITERATIONS` to change the number of runs of each variant of each
//!   scenario (default 100). Run with `--nocapture` to see how many of the model's results
//!   `parking_lot` gave.
//!
//! * [`reference_model_refuses_try_write_behind_two_parked_writers`] checks the model's guard for
//!   `PARKED_BIT`.
//!
//! For the exact model:
//!
//! * [`exact_model_covers_parking_lot_under_stress`] runs the queue scenarios and the first random
//!   programs of `rwlock_parity::programs` many times, as the stress test above runs the scenarios,
//!   and checks each result of `parking_lot` against the exact model, also in the programs where
//!   the model can deadlock.
//! * [`exact_model_wake_filter_matches_parking_lot`] checks which parked requests each release of
//!   the model wakes, and whether it hands the lock to them, against the real releases, on every
//!   queue of up to four parked requests.
//!
//! For the behavior that the models and Shuttle's locks copy, each test shows one point of it on
//! the real `parking_lot`:
//!
//! * [`parking_lot_lets_a_later_writer_overtake_a_parked_one`]: after a plain unlock, a writer that
//!   parked later can get the lock first.
//! * [`parking_lot_strands_a_reader_behind_an_upgradable_read`]: an unlock can leave a reader
//!   parked that only an upgradable read keeps out of the lock, which admits readers, so that a
//!   program deadlocks.
//! * [`parking_lot_upgradable_unlock_fair_does_not_hand_off`] and
//!   [`parking_lot_bump_upgradable_hands_off`]: `unlock_fair` of an upgradable read lock is a plain
//!   unlock in 0.12.5, and its `bump` hands the lock off.
//! * [`parking_lot_downgrades_never_hand_off`].
//! * [`parking_lot_try_write_fails_on_a_free_lock_while_a_writer_is_parked`] and
//!   [`parking_lot_mutex_try_lock_succeeds_while_a_waiter_is_parked`]: `try_write` reads
//!   `PARKED_BIT`, and `Mutex::try_lock` does not.
//!
//! The weekly `beta` job in `.github/workflows/crons.yml` runs these tests too, so that a new
//! `parking_lot` release that changes what they check opens an issue, with more runs of the stress
//! tests.
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
//! The stress tests do not depend on timing, because each order that `parking_lot` gives must be an
//! order of the model. Only their watchdog uses a time limit (see [`STUCK`]), and in the exact
//! stress test, a run of a program in which the model can deadlock gives up after
//! [`DEADLOCK_TIMEOUT`], which only leaves that run unchecked.
//!
//! The overtaking test waits until each writer asks for the lock, and then pauses so that it parks.
//! A stall there, or a run in which eventual fairness hands the lock to W2, only makes a run show no
//! overtaking, and the test then runs again.
//!
//! The tests after it read the park queues of `parking_lot_core` (see [`parked_tokens`]), so they
//! know when a thread is parked without pausing for it, and a slow thread only makes them wait
//! longer, for up to [`DEADLINE`]. Where they need a request that stays parked, or that reports how
//! it was unparked, they park a fake one ([`park_fake`]). What can still depend on timing is
//! eventual fairness, which can make a plain unlock hand the lock off, and the order in which woken
//! threads run. A test whose outcome depends on these runs until it sees the outcome that it looks
//! for, up to [`TRIALS`] times, and nearly always sees it in the first run.

// Each test crate uses only part of the shared module.
#[allow(dead_code)]
mod rwlock_parity;

use parking_lot::{RawRwLock, RwLock, RwLockUpgradableReadGuard, RwLockWriteGuard};
use parking_lot_core::{DEFAULT_PARK_TOKEN, DEFAULT_UNPARK_TOKEN, FilterOp, ParkResult, ParkToken, UnparkToken};
use rwlock_parity::actor::{self, Actor};
use rwlock_parity::exact::{Token, explore_exact, wake_filter, wake_of};
use rwlock_parity::harness::{QUEUED, REQUESTED, TRANSITIONS, describe};
use rwlock_parity::programs::{queue_scenarios, random_programs};
use rwlock_parity::reference::Op::{self, *};
use rwlock_parity::reference::{Holding, Word, explore, step};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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

/// Runs each variant of each `release` scenario of the transition table `iterations` times. Adds 1
/// to `progress` for each run, and keeps the name of the variant in `current`. Returns the
/// mismatches.
fn stress(iterations: u64, progress: &AtomicU64, current: &Mutex<String>) -> Vec<String> {
    const VARIANTS: [Op; 2] = [Unlock, UnlockFair];
    let started = Instant::now();
    let (mut checked, mut skipped, mut mismatches) = (0u64, 0, Vec::new());
    let (mut model_total, mut model_seen) = (0, 0);
    for &main in TRANSITIONS {
        for &queued in QUEUED {
            for &requested in REQUESTED {
                let programs = |unlock: Op| {
                    let mut programs = vec![[main, &[unlock]].concat()];
                    if !queued.is_empty() {
                        programs.push([queued, &[unlock]].concat());
                    }
                    programs.push([requested, &[unlock]].concat());
                    programs
                };
                // The model gives `unlock_fair` the rule of a plain unlock (see the reference module
                // docs), so both variants have the results of the plain one.
                let expected = explore(&programs(Unlock));
                if expected.deadlock {
                    skipped += 1;
                    continue;
                }

                // The values that `parking_lot` gave in either variant.
                let mut seen = BTreeSet::new();
                for (variant, unlock) in VARIANTS.into_iter().enumerate() {
                    let name = format!(
                        "{} | release ({}) | {} | {}",
                        describe(main),
                        unlock.name(),
                        describe(queued),
                        describe(requested)
                    );
                    *current.lock().unwrap() = name.clone();

                    let programs = Arc::new(programs(unlock));
                    let run = checked * VARIANTS.len() as u64 + variant as u64;
                    let mut values = BTreeSet::new();
                    let mut tries = BTreeSet::new();
                    for i in 0..iterations {
                        let (v, t) = real_once(&programs, i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ run);
                        values.insert(v);
                        tries.extend(t);
                        progress.fetch_add(1, Ordering::Relaxed);
                    }

                    let extra_values: Vec<_> = values.difference(&expected.values).collect();
                    let extra_tries: Vec<_> = tries.difference(&expected.try_results).collect();
                    if !extra_values.is_empty() || !extra_tries.is_empty() {
                        mismatches.push(format!(
                            "{name}: parking_lot gave values {extra_values:?} and try results \
                             {extra_tries:?} that the model does not"
                        ));
                    }
                    seen.extend(values);
                }

                checked += 1;
                model_total += expected.values.len();
                model_seen += seen.intersection(&expected.values).count();
            }
        }
    }
    println!(
        "stress: {checked} scenarios x {} variants x {iterations} runs in {:.1?}, {skipped} skipped \
         (the model deadlocks), {} mismatches; parking_lot gave {model_seen} of the model's \
         {model_total} value sets",
        VARIANTS.len(),
        started.elapsed(),
        mismatches.len()
    );
    mismatches
}

#[test]
fn reference_model_covers_parking_lot_under_stress() {
    let iterations = stress_iterations();
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
/// model lets any waiting request win after a plain unlock, as the Shuttle lock does (see the
/// `raw_rwlock` module docs). A plain unlock wakes the first
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

// ---------------------------------------------------------------------------------------------
// The exact model under stress.

/// The random programs (see `rwlock_parity::programs::random_programs`) on which the exact stress
/// test checks the exact model: the first ones from the default seed, of three and of four tasks,
/// which include all that `rwlock_exact_parity` checks Shuttle on by default and in the weekly job.
const EXACT_STRESS_RANDOM_PROGRAMS: usize = 150;

/// The number of programs that the exact stress test runs at the same time. Their threads sleep or
/// park for most of the time, so this number does not depend on the number of CPUs.
const EXACT_STRESS_WORKERS: usize = 8;

/// In a program in which the exact model can deadlock, each blocking op of a run gives up after
/// this long, and so does the run. A run that does not deadlock takes well under a millisecond, and
/// one that only takes longer than this is not checked, which loses nothing that the test asserts.
const DEADLOCK_TIMEOUT: Duration = Duration::from_millis(50);

/// The number of runs of each program in which the exact model can deadlock, for every
/// `DEADLOCK_RUNS_PER` runs of the other programs: a run that deadlocks takes at least
/// [`DEADLOCK_TIMEOUT`].
const DEADLOCK_RUNS_PER: u64 = 5;

/// The number of runs of each variant of each scenario of the stress tests:
/// `RWLOCK_PARITY_STRESS_ITERATIONS`, or 100.
fn stress_iterations() -> u64 {
    std::env::var("RWLOCK_PARITY_STRESS_ITERATIONS").map_or(100, |n| {
        n.parse().expect("RWLOCK_PARITY_STRESS_ITERATIONS must be a number")
    })
}

/// A task of a run gave up: a blocking op or a wait for a signal took longer than the timeout.
struct GaveUp;

/// Runs a task's ops with random delays, like one task of the Shuttle harness, with its end of the
/// program's channel for `Signal` and `AwaitSignal`. With a `timeout`, each blocking op is a timed
/// one, and the task gives up when one of them times out.
fn run_task_with_signals(
    actor: &mut Actor<'_, RawRwLock>,
    ops: &[Op],
    rng: &mut Rng,
    (tx, rx): (Option<&mpsc::Sender<()>>, Option<&mpsc::Receiver<()>>),
    timeout: Option<Duration>,
) -> Result<(), GaveUp> {
    for &op in ops {
        rng.delay();
        match op {
            // The receiver is gone only if the main task gave up.
            Signal => drop(tx.expect("only the other tasks signal").send(())),
            AwaitSignal => {
                let rx = rx.expect("only the main task waits for signals");
                match timeout {
                    Some(timeout) => rx.recv_timeout(timeout).map_err(|_| GaveUp)?,
                    None => rx.recv().expect("each program sends the signal that it waits for"),
                }
            }
            _ => {}
        }
        let Some(timeout) = timeout else {
            actor.step(op);
            continue;
        };
        let mut timed_out = false;
        actor.step_with(op, |guard, lock, op| {
            let (guard, ok) = apply_timed(lock, guard, op, timeout);
            if is_try(op) {
                (guard, ok)
            } else {
                // A blocking op is not a `try_*` op for the results, and it fails only on a
                // timeout.
                timed_out = ok == Some(false);
                (guard, None)
            }
        });
        if timed_out {
            return Err(GaveUp);
        }
    }
    Ok(())
}

/// One run of a program on the real `parking_lot`, in the same order as the Shuttle harness, as
/// [`real_once`] runs a scenario, but with the program's signals. With a `timeout`, see
/// [`run_task_with_signals`]: returns `None` if a task gave up. Without one, a run that deadlocks
/// never ends.
fn real_once_with_signals(programs: &Arc<Vec<Vec<Op>>>, seed: u64, timeout: Option<Duration>) -> Option<RunResult> {
    let lock = Arc::new(RwLock::new(0u8));
    let (tx, rx) = mpsc::channel();
    let mut rng = Rng(seed | 1);
    let mut main = Actor::new(&*lock);
    // A program starts with a blocking request of the main task, on a free lock.
    main.step(programs[0][0]);

    let handles: Vec<_> = (1..programs.len())
        .map(|i| {
            let lock = Arc::clone(&lock);
            let programs = Arc::clone(programs);
            let tx = tx.clone();
            let seed = seed.wrapping_mul(31).wrapping_add(i as u64) | 1;
            thread::spawn(move || {
                let mut actor = Actor::new(&*lock);
                let done = run_task_with_signals(&mut actor, &programs[i], &mut Rng(seed), (Some(&tx), None), timeout);
                done.ok()
                    .map(|()| (std::mem::take(&mut actor.values), std::mem::take(&mut actor.tries)))
            })
        })
        .collect();
    drop(tx);
    let done = run_task_with_signals(&mut main, &programs[0][1..], &mut rng, (None, Some(&rx)), timeout);
    let mut results = vec![
        done.ok()
            .map(|()| (std::mem::take(&mut main.values), std::mem::take(&mut main.tries))),
    ];
    drop(main);
    results.extend(handles.into_iter().map(|h| h.join().unwrap()));

    let (mut values, mut tries) = (Vec::new(), Vec::new());
    for (task, result) in results.into_iter().enumerate() {
        let (task_values, task_tries) = result?;
        values.push(task_values);
        tries.extend(task_tries.into_iter().map(|(op, ok)| (task, op, ok)));
    }
    Some((values, tries))
}

/// What the exact stress test found for one program.
struct ExactStress {
    /// The model can deadlock.
    model_deadlocks: bool,
    /// The runs that gave up, which the test counts as deadlocks.
    deadlocked_runs: u64,
    /// The value sets of the model, and how many of them `parking_lot` gave.
    model_values: usize,
    seen_values: usize,
    /// The results that `parking_lot` gave and the model does not.
    mismatch: Option<String>,
}

/// Runs `programs` on the real `parking_lot` `iterations` times, or `iterations /
/// DEADLOCK_RUNS_PER` times with timed ops where the exact model can deadlock, and compares the
/// results with the model's. Adds 1 to `progress` for each run.
fn exact_stress(name: &str, programs: Vec<Vec<Op>>, iterations: u64, salt: u64, progress: &AtomicU64) -> ExactStress {
    let model = explore_exact(&programs).outcome;
    let (runs, timeout) = if model.deadlock {
        ((iterations / DEADLOCK_RUNS_PER).max(1), Some(DEADLOCK_TIMEOUT))
    } else {
        (iterations, None)
    };
    let programs = Arc::new(programs);
    let (mut values, mut tries, mut deadlocked_runs) = (BTreeSet::new(), BTreeSet::new(), 0);
    for i in 0..runs {
        match real_once_with_signals(&programs, i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ salt, timeout) {
            Some((v, t)) => {
                values.insert(v);
                tries.extend(t);
            }
            None => deadlocked_runs += 1,
        }
        progress.fetch_add(1, Ordering::Relaxed);
    }
    let extra_values: Vec<_> = values.difference(&model.values).collect();
    let extra_tries: Vec<_> = tries.difference(&model.try_results).collect();
    let mismatch = (!extra_values.is_empty() || !extra_tries.is_empty()).then(|| {
        format!(
            "{name} [{}]: parking_lot gave values {extra_values:?} and try results {extra_tries:?} that \
             the exact model does not",
            programs.iter().map(|p| describe(p)).collect::<Vec<_>>().join(" / ")
        )
    });
    ExactStress {
        model_deadlocks: model.deadlock,
        deadlocked_runs,
        model_values: model.values.len(),
        seen_values: values.intersection(&model.values).count(),
        mismatch,
    }
}

/// Every result of the real `parking_lot` must be a result of the exact model
/// (`rwlock_parity::exact`), on the queue scenarios and the first random programs of
/// `rwlock_parity::programs`. This ties the model to `parking_lot` where the park queue matters, as
/// the stress test above ties the reference model to it on the scenarios of the tables.
///
/// Each program runs `RWLOCK_PARITY_STRESS_ITERATIONS` times (100 by default) with random delays.
/// In a program where the model can deadlock, `parking_lot` can deadlock too, so its runs use timed
/// ops (see [`DEADLOCK_TIMEOUT`]), a run that gives up counts as a deadlock, and there are fewer of
/// them. In the other programs, a run that deadlocks fails the test through the watchdog (see
/// [`STUCK`]). The test prints how many of the model's value sets `parking_lot` gave: some need
/// orders that the OS rarely gives.
#[test]
fn exact_model_covers_parking_lot_under_stress() {
    let iterations = stress_iterations();
    let mut programs: Vec<(String, Vec<Vec<Op>>)> = (queue_scenarios().into_iter())
        .map(|s| (s.name.to_string(), s.programs))
        .collect();
    programs.extend(
        (random_programs(0x5eed).take(EXACT_STRESS_RANDOM_PROGRAMS).enumerate())
            .map(|(n, programs)| (format!("random program {n}"), programs)),
    );

    let programs = Arc::new(programs);
    let progress = Arc::new(AtomicU64::new(0));
    let next = Arc::new(AtomicUsize::new(0));
    // The program that each worker runs, for the watchdog.
    let in_flight = Arc::new(Mutex::new(vec![None; EXACT_STRESS_WORKERS]));
    let started = Instant::now();
    // Not scoped threads: a worker in a run that deadlocks never ends, and the watchdog below must
    // be able to fail the test without waiting for it.
    let workers: Vec<_> = (0..EXACT_STRESS_WORKERS)
        .map(|worker| {
            let (programs, progress, next, in_flight) = (
                Arc::clone(&programs),
                Arc::clone(&progress),
                Arc::clone(&next),
                Arc::clone(&in_flight),
            );
            thread::spawn(move || {
                let mut results = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((name, program)) = programs.get(i) else {
                        break;
                    };
                    in_flight.lock().unwrap()[worker] = Some(name.clone());
                    results.push(exact_stress(name, program.clone(), iterations, i as u64, &progress));
                }
                in_flight.lock().unwrap()[worker] = None;
                results
            })
        })
        .collect();

    // A watchdog: a run that deadlocks without a timeout never ends, so the test fails here instead
    // of hanging.
    let (mut seen, mut since) = (0, Instant::now());
    while !workers.iter().all(|w| w.is_finished()) {
        thread::sleep(Duration::from_millis(100));
        let now = progress.load(Ordering::Relaxed);
        if now != seen {
            (seen, since) = (now, Instant::now());
        } else if since.elapsed() > STUCK {
            let stuck: Vec<String> = in_flight.lock().unwrap().iter().flatten().cloned().collect();
            panic!(
                "parking_lot made no progress for {STUCK:?} in {stuck:?}, where the exact model says \
                 that no schedule deadlocks.\n{NEW_RELEASE_HINT}"
            );
        }
    }
    let results: Vec<ExactStress> = workers.into_iter().flat_map(|w| w.join().unwrap()).collect();
    let mismatches: Vec<&str> = results.iter().filter_map(|r| r.mismatch.as_deref()).collect();
    let deadlocking: Vec<&ExactStress> = results.iter().filter(|r| r.model_deadlocks).collect();
    println!(
        "exact stress: {} programs x {iterations} runs ({} where the model can deadlock, x {} runs \
         with timed ops, {} of which deadlocked in parking_lot) in {:.1?}, {} mismatches; \
         parking_lot gave {} of the model's {} value sets",
        results.len(),
        deadlocking.len(),
        (iterations / DEADLOCK_RUNS_PER).max(1),
        deadlocking.iter().filter(|r| r.deadlocked_runs > 0).count(),
        started.elapsed(),
        mismatches.len(),
        results.iter().map(|r| r.seen_values).sum::<usize>(),
        results.iter().map(|r| r.model_values).sum::<usize>(),
    );
    assert!(
        mismatches.is_empty(),
        "parking_lot gave results that the exact model does not:\n{}\n{NEW_RELEASE_HINT}",
        mismatches.join("\n")
    );
}

// ---------------------------------------------------------------------------------------------
// Park queues.
//
// `parking_lot` parks a thread that waits for a lock in a queue of `parking_lot_core`, under the
// address of the raw lock as the key, with a token that says what the thread waits for. The tests
// below read these queues, so that they know when a thread is parked without pausing for it, and
// park fake requests in them, which a release of the lock unparks as it does real ones, but which
// then end instead of trying to take the lock.

/// `parking_lot`'s `TOKEN_NORMAL`: the unparked request tries to take the lock again.
const TOKEN_NORMAL: UnparkToken = UnparkToken(0);
/// `parking_lot`'s `TOKEN_HANDOFF`: the unparked request was handed the lock.
const TOKEN_HANDOFF: UnparkToken = UnparkToken(1);
/// The token with which the tests unpark the fake requests that are left at the end, which
/// `parking_lot` does not use.
const TOKEN_CLEANUP: UnparkToken = UnparkToken(0xc1ea);

/// How often a test looks at a park queue while it waits for a thread to park.
const QUEUE_POLL: Duration = Duration::from_micros(100);

/// The key under which `parking_lot` parks the threads that wait for `lock`.
fn rwlock_key<T>(lock: &RwLock<T>) -> usize {
    // SAFETY: the raw lock is only used for its address, the key of its park queue.
    unsafe { lock.raw() as *const RawRwLock as usize }
}

/// The key under which `parking_lot` parks the threads that wait for `mutex`.
fn mutex_key<T>(mutex: &parking_lot::Mutex<T>) -> usize {
    // SAFETY: as in `rwlock_key`.
    unsafe { mutex.raw() as *const parking_lot::RawMutex as usize }
}

/// The tokens of the threads parked under `key`, front first.
fn parked_tokens(key: usize) -> Vec<usize> {
    // No allocation while the queue is locked.
    let mut tokens = Vec::with_capacity(64);
    // SAFETY: `key` is the key of a lock that the test owns. The filter skips every thread, so this
    // unparks none and leaves the queue as it is. The filter and the callback do not panic or call
    // into `parking_lot`.
    unsafe {
        parking_lot_core::unpark_filter(
            key,
            |ParkToken(token)| {
                tokens.push(token);
                FilterOp::Skip
            },
            |_| DEFAULT_UNPARK_TOKEN,
        );
    }
    tokens
}

/// Waits until the threads parked under `key` have the tokens `expected`, front first. A parked
/// thread stays parked until something unparks it (`parking_lot_core` has no spurious wake-ups on
/// Linux), so the queue cannot change on its own after that.
fn wait_until_parked(key: usize, expected: &[usize]) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let tokens = parked_tokens(key);
        if tokens == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "after {DEADLINE:?}, the parked tokens are {tokens:?}, not {expected:?}"
        );
        thread::sleep(QUEUE_POLL);
    }
}

/// Parks a fake request under `key` with `token`, on its own thread. Returns the thread, which ends
/// when the request is unparked, with the token that it was unparked with.
fn park_fake(key: usize, token: usize) -> thread::JoinHandle<UnparkToken> {
    thread::spawn(move || {
        // SAFETY: `key` is the key of a lock that the test owns, and the threads of the lock take a
        // fake request for a real one. The callbacks do not panic or call into `parking_lot`.
        let result = unsafe { parking_lot_core::park(key, || true, || {}, |_, _| {}, ParkToken(token), None) };
        match result {
            ParkResult::Unparked(token) => token,
            other => unreachable!("a park without validation or timeout returned {other:?}"),
        }
    })
}

/// Parks fake requests under `key` with `tokens`, behind the threads parked there now, in that
/// order: each one is parked before the next one parks.
fn park_fakes(key: usize, tokens: &[usize]) -> Vec<thread::JoinHandle<UnparkToken>> {
    let mut queue = parked_tokens(key);
    (tokens.iter())
        .map(|&token| {
            let fake = park_fake(key, token);
            queue.push(token);
            wait_until_parked(key, &queue);
            fake
        })
        .collect()
}

/// Joins `handle`, or fails the test if its thread does not end within [`DEADLINE`], and leaves the
/// thread to the end of the process: a thread that `parking_lot` leaves parked would otherwise hang
/// the test.
fn join<T>(handle: thread::JoinHandle<T>) -> T {
    let deadline = Instant::now() + DEADLINE;
    while !handle.is_finished() {
        assert!(
            Instant::now() < deadline,
            "a thread of the test did not end in {DEADLINE:?}.\n{NEW_RELEASE_HINT}"
        );
        thread::sleep(QUEUE_POLL);
    }
    handle.join().unwrap()
}

/// Unparks the requests parked under `key` with [`TOKEN_CLEANUP`], which ends the fake ones.
fn unpark_the_rest(key: usize) {
    // SAFETY: `key` is the key of a lock that the test owns, and only fake requests are parked
    // there, which end when they are unparked.
    unsafe { parking_lot_core::unpark_all(key, TOKEN_CLEANUP) };
}

/// Sets `PARKED_BIT` of `lock`, which must be held in a mode that keeps writers out, with fake
/// requests parked under its key. A fake request parks without setting the bit, so without this, a
/// release would not look at the queue. A timed `write` sets the bit, parks behind the fakes, and
/// times out, and as other threads are still parked under the key, its time-out leaves the bit set.
fn set_parked_bit<T: Send + Sync>(lock: &RwLock<T>) {
    thread::scope(|s| {
        let writer = s.spawn(|| lock.try_write_for(Duration::from_millis(1)).is_none());
        assert!(writer.join().unwrap(), "a `write` got a lock that keeps writers out");
    });
}

/// Unparks the front request under `key`, which must be a fake one, so that `parking_lot_core`
/// computes `be_fair` for the bucket of `key`. If the bucket's timer of eventual fairness has run
/// out, this restarts it, at 0 to 1 ms. So the next release of the lock, right after this, is very
/// likely not made fair by the timer.
fn restart_fairness_timer(key: usize) {
    // SAFETY: `key` is the key of a lock that the test owns, and its front request is a fake one,
    // which ends when it is unparked. The callback does not panic or call into `parking_lot`.
    let result = unsafe { parking_lot_core::unpark_one(key, |_| TOKEN_CLEANUP) };
    assert_eq!(result.unparked_threads, 1, "no request was parked under the key");
}

// ---------------------------------------------------------------------------------------------
// The wake filter.

/// A release of a lock that wakes the main queue, from the mode in which a thread holds the lock.
#[derive(Clone, Copy, Debug)]
enum Release {
    Unlock,
    UnlockFair,
    Downgrade,
    DowngradeToUpgradable,
    UnlockUpgradable,
    UnlockUpgradableFair,
    DowngradeUpgradable,
}

impl Release {
    const ALL: [Release; 7] = [
        Release::Unlock,
        Release::UnlockFair,
        Release::Downgrade,
        Release::DowngradeToUpgradable,
        Release::UnlockUpgradable,
        Release::UnlockUpgradableFair,
        Release::DowngradeUpgradable,
    ];

    /// The op of the models, and the mode that it releases.
    fn model(self) -> (Op, Holding) {
        match self {
            Release::Unlock => (Unlock, Holding::Exclusive),
            Release::UnlockFair => (UnlockFair, Holding::Exclusive),
            Release::Downgrade => (Downgrade, Holding::Exclusive),
            Release::DowngradeToUpgradable => (DowngradeToUpgradable, Holding::Exclusive),
            Release::UnlockUpgradable => (Unlock, Holding::Upgradable),
            Release::UnlockUpgradableFair => (UnlockFair, Holding::Upgradable),
            Release::DowngradeUpgradable => (DowngradeUpgradable, Holding::Upgradable),
        }
    }

    /// The token with which the release unparks requests whatever eventual fairness says, if there
    /// is one: `unlock_fair` of a write lock always hands the lock off, and the downgrades never
    /// do. The other releases hand it off when the timer of eventual fairness has run out.
    fn always(self) -> Option<UnparkToken> {
        match self {
            Release::UnlockFair => Some(TOKEN_HANDOFF),
            Release::Downgrade | Release::DowngradeToUpgradable | Release::DowngradeUpgradable => Some(TOKEN_NORMAL),
            Release::Unlock | Release::UnlockUpgradable | Release::UnlockUpgradableFair => None,
        }
    }

    /// The request that takes the lock in the mode that the release releases.
    fn request(self) -> Op {
        match self.model().1 {
            Holding::Exclusive => Write,
            _ => UpgradableRead,
        }
    }
}

/// The request that parks with `token`.
fn request_of(token: Token) -> Op {
    match token {
        Token::Shared => Read,
        Token::Upgradable => UpgradableRead,
        Token::Exclusive => Write,
    }
}

/// Runs `release` on a lock while fake requests with `tokens` are parked, front first. Returns how
/// the requests that it unparks, those that it leaves parked, and the tokens that it unparks them
/// with differ from what the exact model says, if they do.
fn check_wake(release: Release, tokens: &[usize]) -> Result<(), String> {
    let lock = RwLock::new(0u8);
    let key = rwlock_key(&lock);
    let (guard, _) = Guard::Nothing.apply(&lock, release.request());
    let fakes = park_fakes(key, tokens);
    set_parked_bit(&lock);
    let (op, holding) = release.model();
    let (guard, _) = guard.apply(&lock, op);
    let left = parked_tokens(key);
    unpark_the_rest(key);
    // The lock is not used again: the thread gives up what it still holds, and if the release
    // handed the lock to fake requests, they keep it.
    drop(guard);
    let received: Vec<UnparkToken> = fakes.into_iter().map(join).collect();

    let wake = wake_of(op, holding).expect("each of these releases wakes the main queue");
    let expected = wake_filter(tokens, wake.start);
    let unparked: Vec<usize> = (0..tokens.len()).filter(|&i| received[i] != TOKEN_CLEANUP).collect();
    let expected_left: Vec<usize> = (0..tokens.len())
        .filter(|i| !expected.contains(i))
        .map(|i| tokens[i])
        .collect();
    let mut errors = Vec::new();
    if unparked != expected {
        errors.push(format!("unparked the requests at {unparked:?}, the model {expected:?}"));
    }
    if left != expected_left {
        errors.push(format!("left {left:?} parked, the model {expected_left:?}"));
    }
    let tokens_received: Vec<usize> = unparked.iter().map(|&i| received[i].0).collect();
    if let Some(always) = release.always()
        && tokens_received.iter().any(|&token| token != always.0)
    {
        errors.push(format!(
            "unparked them with the tokens {tokens_received:?}, not always {}",
            always.0
        ));
    }
    if wake.hand_off != (release.always() == Some(TOKEN_HANDOFF)) {
        errors.push(format!("the model says that it hands the lock off: {}", wake.hand_off));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("{release:?} with {tokens:?} parked: {}", errors.join(", ")))
    }
}

/// The exact model wakes the main queue as the real `parking_lot` does: `rwlock_parity::exact`'s
/// `wake_filter`, a copy of the filter of `wake_parked_threads`, and its `wake_of`, which says
/// where each release starts the filter and whether it hands the lock off.
///
/// First, real requests park with the tokens of the model. Then, for each release that wakes the
/// main queue and each queue of one to four parked requests of the three kinds, a release of the
/// real lock must unpark the requests that the model says, leave the others parked in their order,
/// and hand the lock to the requests that it unparks if the model says so: `unlock_fair` of a write
/// lock always does, and the downgrades never do. The other releases hand the lock off when the
/// timer of eventual fairness has run out, which the model leaves out (see its module docs).
///
/// The parked requests are fake ones (see [`park_fake`]), which end when they are unparked instead
/// of trying to take the lock, so the queue is known at each release, and the test does not depend
/// on timing.
#[test]
fn exact_model_wake_filter_matches_parking_lot() {
    {
        let lock = Arc::new(RwLock::new(0u8));
        let key = rwlock_key(&lock);
        let held = lock.write();
        let mut queue = Vec::new();
        let requests: Vec<_> = (Token::ALL.into_iter())
            .map(|token| {
                let lock = Arc::clone(&lock);
                let request = thread::spawn(move || drop(Guard::Nothing.apply(&lock, request_of(token))));
                queue.push(token.value());
                wait_until_parked(key, &queue);
                request
            })
            .collect();
        drop(held);
        for request in requests {
            join(request);
        }
    }

    let mut queues: Vec<Vec<usize>> = vec![vec![]];
    let mut cases = Vec::new();
    for _ in 1..=4 {
        queues = (queues.iter())
            .flat_map(|queue| Token::ALL.map(|token| [queue.as_slice(), &[token.value()]].concat()))
            .collect();
        cases.extend(
            (Release::ALL.iter()).flat_map(|&release| queues.iter().map(move |queue| (release, queue.clone()))),
        );
    }
    // `FIXED_WORKERS` cases at a time, each on its own lock.
    let next = AtomicUsize::new(0);
    let errors = Mutex::new(Vec::new());
    thread::scope(|s| {
        for _ in 0..FIXED_WORKERS {
            s.spawn(|| {
                while let Some((release, tokens)) = cases.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if let Err(why) = check_wake(*release, tokens) {
                        errors.lock().unwrap().push(why);
                    }
                }
            });
        }
    });
    let errors = errors.into_inner().unwrap();
    println!("wake filter: {} cases, {} mismatches", cases.len(), errors.len());
    assert!(
        errors.is_empty(),
        "the exact model wakes the queue differently from parking_lot:\n{}\n{NEW_RELEASE_HINT}",
        errors.join("\n")
    );
}

// ---------------------------------------------------------------------------------------------
// What the models and Shuttle's locks copy of `parking_lot`.

/// The runs of each test below whose outcome depends on timing. Such a test runs until the outcome
/// that it looks for happens, which is very likely in the first run, and fails only if it never
/// does.
const TRIALS: usize = 50;

/// The runs of each test below whose outcome does not depend on timing.
const RUNS: usize = 10;

/// Longer than the timer of eventual fairness runs after it restarts (at most 1 ms), so that after
/// this pause, it has run out.
const FAIRNESS_TIMER_RUN_OUT: Duration = Duration::from_millis(3);

/// The park tokens, as the queues show them.
const S: usize = Token::Shared.value();
const X: usize = Token::Exclusive.value();

/// One run of [`parking_lot_strands_a_reader_behind_an_upgradable_read`]. Returns whether the
/// reader was stranded.
fn strands_a_reader(read_cycles: usize) -> bool {
    let lock = Arc::new(RwLock::new(0u8));
    let key = rwlock_key(&lock);
    let held = lock.write();
    let reader_in = Arc::new(AtomicBool::new(false));
    let (upgradable_tx, upgradable_rx) = mpsc::channel();
    let (reader_tx, reader_rx) = mpsc::channel();
    // W1 takes the lock back right after its unlock, ends with an upgradable read, and holds it
    // until the reader is in.
    let w1 = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || {
            drop(lock.write());
            for _ in 0..read_cycles {
                drop(lock.read());
            }
            let upgradable = lock.upgradable_read();
            upgradable_tx.send(()).unwrap();
            reader_rx.recv().unwrap();
            drop(upgradable);
        })
    };
    wait_until_parked(key, &[X]);
    let w2 = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || drop(lock.write()))
    };
    wait_until_parked(key, &[X, X]);
    let reader = {
        let (lock, reader_in) = (Arc::clone(&lock), Arc::clone(&reader_in));
        thread::spawn(move || {
            let _read = lock.read();
            reader_in.store(true, Ordering::SeqCst);
            reader_tx.send(()).unwrap();
        })
    };
    wait_until_parked(key, &[X, X, S]);
    drop(held);
    upgradable_rx
        .recv_timeout(DEADLINE)
        .expect("W1 gets its upgradable read");

    // Stranded: the reader is parked, with W2 behind it, which lost the race to W1's read cycles
    // and upgradable read and parked again. Then no thread can unpark the reader: W1 waits for it,
    // and W2 waits behind it.
    let deadline = Instant::now() + DEADLINE;
    let stranded = loop {
        if reader_in.load(Ordering::SeqCst) {
            break false;
        }
        if parked_tokens(key) == [S, X] {
            break true;
        }
        assert!(
            Instant::now() < deadline,
            "the reader is neither in nor stranded after {DEADLINE:?}: {:?} parked",
            parked_tokens(key)
        );
        thread::sleep(QUEUE_POLL);
    };
    if stranded {
        assert!(
            !lock.is_locked_exclusive(),
            "the stranded reader waits for a lock that admits readers"
        );
        // Unpark the reader, which no thread of the run would do, so that the threads end.
        // SAFETY: `key` is the key of the test's lock. The reader tries to take the lock again, as
        // after a plain unlock. The filter and the callback do not panic or call into
        // `parking_lot`.
        unsafe {
            parking_lot_core::unpark_filter(
                key,
                |ParkToken(token)| if token == S { FilterOp::Unpark } else { FilterOp::Skip },
                |_| TOKEN_NORMAL,
            );
        }
    }
    for thread in [w1, w2, reader] {
        join(thread);
    }
    stranded
}

/// `parking_lot` strands a reader as the `raw_rwlock` module docs say, and as the exact model's
/// "stranded reader" queue scenarios do: with no read cycle, and with two read cycles between W1's
/// write unlock and its upgradable read.
///
/// W1, W2 and a reader park in that order behind a write lock. Its unlock wakes only W1, the
/// first writer, which then unlocks at once, which wakes only W2. Before W2 runs, W1 does its read
/// cycles, whose shared unlocks wake nobody, and takes an upgradable read, so W2 finds the lock
/// taken and parks again, behind the reader. The upgradable read admits readers, but the reader
/// stays parked, and W1 waits for it: a deadlock. The test sees it in the park queue, which then
/// holds the reader and W2, and lets the reader go itself.
///
/// With two read cycles, the woken W2 is overtaken twice before it loses the race, which Shuttle's
/// lock allows only with a `Config::parking_lot_fairness_bound` above the default of 2 (see the
/// "stranded reader after two read cycles" queue scenario of `rwlock_exact_parity`). In a run where
/// eventual fairness makes W1's unlock hand the lock to W2, the reader gets in, and the test runs
/// again.
#[test]
fn parking_lot_strands_a_reader_behind_an_upgradable_read() {
    for read_cycles in [0, 2] {
        let run = (1..=TRIALS).find(|_| strands_a_reader(read_cycles));
        let Some(run) = run else {
            panic!("with {read_cycles} read cycles, the reader got in in all {TRIALS} runs.\n{NEW_RELEASE_HINT}");
        };
        println!("with {read_cycles} read cycles, the reader was stranded in run {run}");
    }
}

/// In `parking_lot` 0.12.5, `RwLockUpgradableReadGuard::unlock_fair` does not hand the lock to the
/// writer that it wakes, unlike `RwLockWriteGuard::unlock_fair`: it passes `force_fair = false`. So
/// the thread that unlocked can take the lock again before the woken writer tries. The exact model
/// and Shuttle's lock treat it as a plain unlock.
///
/// The writer parks behind a fake request, which the test unparks right before the unlock, so that
/// the timer of eventual fairness restarts (see [`restart_fairness_timer`]): the main thread waited
/// for the writer to park, so the timer has most likely run out, and would make the unlock a
/// hand-off. The test runs until the main thread's `try_write` right after the unlock succeeds,
/// which a hand-off makes impossible.
#[test]
fn parking_lot_upgradable_unlock_fair_does_not_hand_off() {
    for run in 1..=TRIALS {
        let lock = Arc::new(RwLock::new(0u8));
        let key = rwlock_key(&lock);
        let upgradable = lock.upgradable_read();
        let fake = park_fakes(key, &[S]);
        let writer = {
            let lock = Arc::clone(&lock);
            thread::spawn(move || drop(lock.write()))
        };
        wait_until_parked(key, &[S, X]);
        restart_fairness_timer(key);
        RwLockUpgradableReadGuard::unlock_fair(upgradable);
        let barged = lock.try_write().is_some();
        for thread in fake {
            assert_eq!(join(thread), TOKEN_CLEANUP);
        }
        join(writer);
        if barged {
            println!("the unlocking thread took the lock first in run {run}");
            return;
        }
    }
    panic!(
        "the writer had the lock right after `RwLockUpgradableReadGuard::unlock_fair` in all {TRIALS} \
         runs, as after a hand-off.\n{NEW_RELEASE_HINT}"
    );
}

/// `RwLockUpgradableReadGuard::bump` hands the lock to a parked writer, unlike `unlock_fair` of the
/// same guard: the writer gets the lock before the bumping thread gets its upgradable read back.
/// The writer adds 1 under its lock, so the bumping thread sees 1 when `bump` returns. This does
/// not depend on timing: after a hand-off, the bumping thread waits for the writer.
#[test]
fn parking_lot_bump_upgradable_hands_off() {
    for _ in 0..RUNS {
        let lock = Arc::new(RwLock::new(0u8));
        let key = rwlock_key(&lock);
        let mut upgradable = lock.upgradable_read();
        let writer = {
            let lock = Arc::clone(&lock);
            thread::spawn(move || *lock.write() += 1)
        };
        wait_until_parked(key, &[X]);
        RwLockUpgradableReadGuard::bump(&mut upgradable);
        assert_eq!(
            *upgradable, 1,
            "the writer did not get the lock during `bump`.\n{NEW_RELEASE_HINT}"
        );
        drop(upgradable);
        join(writer);
    }
}

/// The token with which `release` of a lock unparks a fake request with `token`, parked alone,
/// after a pause in which the timer of eventual fairness runs out.
fn unpark_token_after_a_pause(release: Release, token: usize) -> UnparkToken {
    let lock = RwLock::new(0u8);
    let key = rwlock_key(&lock);
    let (guard, _) = Guard::Nothing.apply(&lock, release.request());
    let fake = park_fake(key, token);
    wait_until_parked(key, &[token]);
    set_parked_bit(&lock);
    thread::sleep(FAIRNESS_TIMER_RUN_OUT);
    let (guard, _) = guard.apply(&lock, release.model().0);
    let left = parked_tokens(key);
    unpark_the_rest(key);
    drop(guard);
    assert!(left.is_empty(), "{release:?} did not unpark the request");
    join(fake)
}

/// The downgrades of `parking_lot` wake the parked requests that the lock then admits, but never
/// hand them the lock, not even when the timer of eventual fairness has run out, as the exact model
/// and Shuttle's lock have it (`parking_lot` ignores `be_fair` there). The request is a fake one,
/// which reports the token that it was unparked with: `TOKEN_NORMAL`, never `TOKEN_HANDOFF`.
///
/// Before each downgrade, the test pauses until the timer has run out. As a control, a plain unlock
/// after the same pause hands the lock off, in at least one run: the timer is per bucket of
/// `parking_lot_core`'s table, so another test in the same process can restart it.
#[test]
fn parking_lot_downgrades_never_hand_off() {
    // Each downgrade, and a request that it wakes.
    let downgrades = [
        (Release::Downgrade, X),
        (Release::DowngradeToUpgradable, S),
        (Release::DowngradeUpgradable, X),
    ];
    for (release, token) in downgrades {
        for _ in 0..RUNS {
            assert_eq!(
                unpark_token_after_a_pause(release, token),
                TOKEN_NORMAL,
                "{release:?} handed the lock off.\n{NEW_RELEASE_HINT}"
            );
        }
    }
    let run = (1..=TRIALS).find(|_| unpark_token_after_a_pause(Release::Unlock, X) == TOKEN_HANDOFF);
    let Some(run) = run else {
        panic!(
            "after the pause, a plain unlock did not hand the lock off in {TRIALS} runs, so the pause \
             does not make the timer run out"
        );
    };
    println!("after the pause, a plain unlock handed the lock off in run {run}");
}

/// `try_write` fails on a free lock while a writer is parked, because it is a compare-and-swap of
/// the lock word from 0, and `PARKED_BIT` is set. `try_read` and `try_upgradable_read` succeed. The
/// exact model, and Shuttle's lock (`try_acquire_without_overtaking`), have the same.
///
/// A fake request and then a writer park behind a write lock. The holder downgrades, which wakes
/// only the fake request, the first writer, and leaves the writer parked, and then unlocks its read
/// lock, which wakes nobody. This does not depend on timing.
#[test]
fn parking_lot_try_write_fails_on_a_free_lock_while_a_writer_is_parked() {
    let lock = Arc::new(RwLock::new(0u8));
    let key = rwlock_key(&lock);
    let held = lock.write();
    let fake = park_fake(key, X);
    wait_until_parked(key, &[X]);
    let writer = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || drop(lock.write()))
    };
    wait_until_parked(key, &[X, X]);
    let read = RwLockWriteGuard::downgrade(held);
    assert_eq!(join(fake), TOKEN_NORMAL);
    drop(read);

    assert_eq!(parked_tokens(key), [X], "the writer is parked");
    assert!(!lock.is_locked(), "the lock is free");
    assert!(
        lock.try_write().is_none(),
        "try_write succeeded while a writer is parked.\n{NEW_RELEASE_HINT}"
    );
    assert!(lock.try_read().is_some(), "try_read failed on a free lock");
    // Its unlock wakes the writer.
    let upgradable = lock.try_upgradable_read();
    assert!(upgradable.is_some(), "try_upgradable_read failed on a free lock");
    drop(upgradable);
    join(writer);
}

/// A `Mutex`'s `try_lock` succeeds on a free mutex while a waiter is parked: unlike `RwLock`'s
/// `try_write`, it ignores `PARKED_BIT`. Shuttle's `Mutex` (`try_acquire`) has the same.
///
/// Two fake requests and then a waiter park behind the mutex. The test unparks the first fake
/// request to restart the timer of eventual fairness (see [`restart_fairness_timer`]), and then
/// unlocks, which wakes the second one and leaves the waiter parked. In a run where eventual
/// fairness still makes the unlock hand the mutex to the fake request, the test hands it on to the
/// waiter and runs again.
#[test]
fn parking_lot_mutex_try_lock_succeeds_while_a_waiter_is_parked() {
    const PARKED: usize = DEFAULT_PARK_TOKEN.0;
    for run in 1..=TRIALS {
        let mutex = Arc::new(parking_lot::Mutex::new(()));
        let key = mutex_key(&mutex);
        let held = mutex.lock();
        let mut fakes = park_fakes(key, &[PARKED, PARKED]);
        let waiter = {
            let mutex = Arc::clone(&mutex);
            thread::spawn(move || drop(mutex.lock()))
        };
        wait_until_parked(key, &[PARKED; 3]);
        restart_fairness_timer(key);
        drop(held);
        let woken = join(fakes.pop().unwrap());
        assert_eq!(join(fakes.pop().unwrap()), TOKEN_CLEANUP);
        if woken == TOKEN_HANDOFF {
            // SAFETY: `key` is the key of the test's mutex, which the fake request holds after the
            // hand-off, and never unlocks. Handing it on to the waiter, its only other user, keeps
            // it locked by one thread. The callback does not panic or call into `parking_lot`.
            unsafe { parking_lot_core::unpark_one(key, |_| TOKEN_HANDOFF) };
            join(waiter);
            continue;
        }
        assert_eq!(parked_tokens(key), [PARKED], "the waiter is parked");
        assert!(!mutex.is_locked(), "the mutex is free");
        let guard = mutex.try_lock();
        assert!(
            guard.is_some(),
            "try_lock failed on a free mutex while a waiter is parked.\n{NEW_RELEASE_HINT}"
        );
        // Its unlock wakes the waiter.
        drop(guard);
        join(waiter);
        println!("the unlock left the mutex free in run {run}");
        return;
    }
    panic!("eventual fairness made the unlock a hand-off in all {TRIALS} runs");
}
