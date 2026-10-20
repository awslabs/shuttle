//! The programs on which the exact model (`super::exact`) is checked, against Shuttle in
//! `rwlock_exact_parity` and against the real `parking_lot` in `rwlock_reference_model`: queue
//! scenarios, in which requests park behind the lock, and random programs.
//!
//! A program has one list of ops for each task, as in the parity tables (see `super::harness`):
//! task 0 runs its first op, starts the other tasks, and then runs the rest of its ops. Only task 0
//! waits for signals, and only the other tasks send them.

use super::reference::Op::{self, *};

/// What Shuttle's lock reaches of the exact model's outcomes at its default fairness bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtDefaultBound {
    /// Every outcome of the model.
    Every,
    /// Not every outcome: the bound lets a woken request in before the schedules that give the
    /// others, which Shuttle reaches without a bound.
    Some,
}

/// A program in which requests park behind the lock, so that its outcomes can depend on the order
/// in which they park and on which of them a release wakes, with what the model and Shuttle give.
pub struct QueueScenario {
    pub name: &'static str,
    pub programs: Vec<Vec<Op>>,
    /// Whether the exact model deadlocks.
    pub deadlocks: bool,
    pub at_default_bound: AtDefaultBound,
}

/// The stranded reader (`raw_rwlock` module docs) with `read_cycles` read locks between the
/// writer's unlock and its upgradable read. The main task holds a write lock while a writer and
/// then a reader park, in either order. If the writer parks first, the unlock wakes only the
/// writer, the main task takes the read locks and the upgradable read before the writer tries
/// again, and the writer parks again behind the reader. The reader stays parked while only the
/// upgradable read is held, and the main task waits for it.
fn stranded_reader(read_cycles: usize) -> Vec<Vec<Op>> {
    let mut main = vec![Write, Unlock];
    for _ in 0..read_cycles {
        main.extend([Read, Unlock]);
    }
    main.extend([UpgradableRead, AwaitSignal, Unlock]);
    vec![main, vec![Write, Unlock], vec![Read, Signal, Unlock]]
}

pub fn queue_scenarios() -> Vec<QueueScenario> {
    let scenario = |name, programs, deadlocks, at_default_bound| QueueScenario {
        name,
        programs,
        deadlocks,
        at_default_bound,
    };
    use AtDefaultBound::{Every, Some};
    vec![
        scenario("stranded reader", stranded_reader(0), true, Every),
        // Each read unlock overtakes the woken writer once. With one, the writer has one overtake
        // when it loses the race to the upgradable read, below the default bound of 2.
        scenario("stranded reader after a read cycle", stranded_reader(1), true, Every),
        // With two, the bound lets the writer in at the second read unlock, before the upgradable
        // read, so Shuttle finds this deadlock only with a larger bound (see the `raw_rwlock`
        // module docs).
        scenario("stranded reader after two read cycles", stranded_reader(2), true, Some),
        // The fair unlock hands the lock to the writer, which then wakes the reader.
        scenario(
            "stranded reader, unlock_fair",
            vec![
                vec![Write, UnlockFair, UpgradableRead, AwaitSignal, Unlock],
                vec![Write, Unlock],
                vec![Read, Signal, Unlock],
            ],
            false,
            Every,
        ),
        // The unlock wakes one of the writers, and the other one stays parked, so the `try_write`
        // can fail on a free lock. The read after it then sees the value from before the writers,
        // which it cannot see after a `try_write` that a writer in the lock made fail. A thread can
        // always wait before it asks for the lock instead of parking, so only an outcome like this
        // one shows that requests park.
        scenario(
            "try_write behind two parked writers",
            vec![
                vec![Write, Unlock, TryWrite, Unlock, Read, Unlock],
                vec![Write, Unlock],
                vec![Write, Unlock],
            ],
            false,
            Every,
        ),
        // The woken writer races with the main task's next `write`, and if it loses, it parks
        // again behind the other writer.
        scenario(
            "relocking writer vs two writers",
            vec![
                vec![Write, Unlock, Write, Unlock],
                vec![Write, Unlock],
                vec![Write, Unlock],
            ],
            false,
            Every,
        ),
        scenario(
            "relocking writer vs two writers, unlock_fair",
            vec![
                vec![Write, UnlockFair, Write, UnlockFair],
                vec![Write, Unlock],
                vec![Write, Unlock],
            ],
            false,
            Every,
        ),
        scenario(
            "upgradable relock vs a writer and a reader",
            vec![
                vec![UpgradableRead, Unlock, UpgradableRead, Unlock],
                vec![Write, Unlock],
                vec![Read, Unlock],
            ],
            false,
            Every,
        ),
        // `unlock_fair` of an upgradable read lock is a plain unlock in `parking_lot` 0.12.5: it
        // wakes the first writer, and leaves the lock free while the other one stays parked, so
        // that the `try_write` can fail and the read get in before the writers, as above. A
        // hand-off would give the woken writer the lock first.
        scenario(
            "upgradable unlock_fair vs two writers",
            vec![
                vec![UpgradableRead, UnlockFair, TryWrite, Unlock, Read, Unlock],
                vec![Write, Unlock],
                vec![Write, Unlock],
            ],
            false,
            Every,
        ),
        scenario(
            "downgrade with a writer and a reader",
            vec![
                vec![Write, Downgrade, Unlock, TryWrite, Unlock],
                vec![Write, Unlock],
                vec![Read, Unlock],
            ],
            false,
            Every,
        ),
        // A downgrade wakes the first writer without a hand-off, as above.
        scenario(
            "downgrade with two writers",
            vec![
                vec![Write, Downgrade, Unlock, TryWrite, Unlock, Read, Unlock],
                vec![Write, Unlock],
                vec![Write, Unlock],
            ],
            false,
            Every,
        ),
        scenario(
            "downgrade_to_upgradable relock",
            vec![
                vec![Write, DowngradeToUpgradable, Unlock, Write, Unlock],
                vec![Write, Unlock],
                vec![Read, Unlock],
            ],
            false,
            Every,
        ),
        // Three requests park behind the write lock, in every order, and the unlock wakes the front
        // ones up to the first writer. Four tasks are slow under DFS, so these programs are short.
        scenario(
            "two writers and a reader parked",
            vec![
                vec![Write, Unlock],
                vec![Write, Unlock],
                vec![Read, Unlock],
                vec![Write, Unlock],
            ],
            false,
            Every,
        ),
        // After an upgradable reader, the filter skips the writer and goes on to the reader.
        scenario(
            "a writer, a reader and an upgradable reader parked",
            vec![
                vec![Write, Unlock],
                vec![Write, Unlock],
                vec![Read, Unlock],
                vec![UpgradableRead, Unlock],
            ],
            false,
            Every,
        ),
    ]
}

/// The number of random programs of three tasks that Shuttle is checked against the exact model on:
/// `RWLOCK_EXACT_PARITY_PROGRAMS`, or 30.
pub fn three_task_program_count() -> usize {
    count_from_env("RWLOCK_EXACT_PARITY_PROGRAMS", 30)
}

/// The number of random programs of four tasks that Shuttle is checked against the exact model on:
/// `RWLOCK_EXACT_PARITY_FOUR_TASK_PROGRAMS`, or none. Exploring every schedule of four tasks takes
/// Shuttle up to a minute for one program, so only the weekly `beta` job in
/// `.github/workflows/crons.yml` sets this.
pub fn four_task_program_count() -> usize {
    count_from_env("RWLOCK_EXACT_PARITY_FOUR_TASK_PROGRAMS", 0)
}

fn count_from_env(name: &str, default: usize) -> usize {
    std::env::var(name).map_or(default, |n| {
        n.parse().unwrap_or_else(|_| panic!("{name} must be a number"))
    })
}

/// The seed of the random programs: `RWLOCK_EXACT_PARITY_SEED`, or a fixed one, so that each run
/// checks the same programs.
pub fn random_seed() -> u64 {
    std::env::var("RWLOCK_EXACT_PARITY_SEED").map_or(0x5eed, |n| {
        n.parse().expect("RWLOCK_EXACT_PARITY_SEED must be a number")
    })
}

/// The first `count` random programs of `tasks` tasks from `seed` (see [`random_programs`]), with
/// their indices in the sequence.
pub fn random_programs_of(tasks: usize, count: usize, seed: u64) -> Vec<(usize, Vec<Vec<Op>>)> {
    (random_programs(seed).enumerate())
        .filter(|(_, programs)| programs.len() == tasks)
        .take(count)
        .collect()
}

/// A small xorshift generator, so that the tests need no other crate.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len())]
    }
}

/// The random programs from `seed`, without end. Every fifth one has four tasks of two ops or so,
/// the others three tasks of two to four ops or so. In a third of them, task 0 waits at a random
/// point for a signal that a random other task sends at a random point.
pub fn random_programs(seed: u64) -> impl Iterator<Item = Vec<Vec<Op>>> {
    let mut rng = Rng(seed | 1);
    (0..).map(move |n| {
        let tasks = if n % 5 == 4 { 4 } else { 3 };
        let mut programs: Vec<Vec<Op>> = (0..tasks)
            .map(|task| {
                let len = if tasks == 4 { 2 } else { 2 + rng.below(3) };
                random_program(&mut rng, len, task == 0)
            })
            .collect();
        if rng.below(3) == 0 {
            let at = 1 + rng.below(programs[0].len() - 1);
            programs[0].insert(at, AwaitSignal);
            let other = 1 + rng.below(tasks - 1);
            let len = programs[other].len();
            let at = (1 + rng.below(len)).min(len);
            programs[other].insert(at, Signal);
        }
        programs
    })
}

/// A random valid program of about `len` ops, which ends holding nothing. Task 0 (`main`) starts
/// with a blocking request, which it makes before the other tasks start.
fn random_program(rng: &mut Rng, len: usize, main: bool) -> Vec<Op> {
    /// What the task holds after its last op. `Any` is after a `try_*`, which may have failed.
    #[derive(Clone, Copy, PartialEq)]
    enum Held {
        Nothing,
        Shared,
        Upgradable,
        Exclusive,
        Any,
    }
    let mut ops = Vec::new();
    let mut held = Held::Nothing;
    while ops.len() < len || held != Held::Nothing {
        let op = match held {
            Held::Nothing => rng.pick(&[
                Read,
                UpgradableRead,
                Write,
                Write,
                Read,
                UpgradableRead,
                TryRead,
                TryUpgradableRead,
                TryWrite,
            ]),
            Held::Shared => rng.pick(&[Unlock, Unlock, UnlockFair]),
            Held::Upgradable => rng.pick(&[Upgrade, TryUpgrade, DowngradeUpgradable, Unlock, UnlockFair, Unlock]),
            Held::Exclusive => rng.pick(&[Downgrade, DowngradeToUpgradable, Unlock, UnlockFair, Unlock]),
            Held::Any => rng.pick(&[Unlock, UnlockFair]),
        };
        if ops.is_empty() && main && matches!(op, TryRead | TryUpgradableRead | TryWrite) {
            continue;
        }
        held = match op {
            Read | Downgrade | DowngradeUpgradable => Held::Shared,
            UpgradableRead | DowngradeToUpgradable => Held::Upgradable,
            Write | Upgrade => Held::Exclusive,
            TryRead | TryUpgradableRead | TryWrite | TryUpgrade => Held::Any,
            Unlock | UnlockFair => Held::Nothing,
            Signal | AwaitSignal => unreachable!("not picked"),
        };
        ops.push(op);
        if ops.len() > len + 4 {
            if held != Held::Nothing {
                ops.push(Unlock);
            }
            break;
        }
    }
    ops
}
