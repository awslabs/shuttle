use rand::rngs::OsRng;
use rand::{RngCore, SeedableRng};
use rand_pcg::Pcg64Mcg;
use shuttle_engine::runtime::task::{Task, TaskId};
use shuttle_engine::scheduler::data::random::RandomDataSource;
use shuttle_engine::scheduler::data::DataSource;
use shuttle_engine::scheduler::{Schedule, Scheduler};
use shuttle_engine::seed_from_env;

/// A scheduler that implements Partial Order Sampling (POS), from "Partial Order Aware Concurrency
/// Sampling" (Yuan, Yang and Gu, CAV 2018).
///
/// A random walk ([`RandomScheduler`](crate::RandomScheduler)) picks uniformly among the runnable
/// tasks at every step. It therefore samples many different interleavings of operations that
/// commute, and only rarely the orderings of the operations that don't. For example, if one task
/// does `n` operations of its own before writing a shared variable, and another task reads that
/// variable once, a random walk runs the read after the write with probability `2^-(n+1)`.
///
/// POS gives every runnable task a random priority, and always runs the task with the highest one.
/// Once a task has run, it gets a fresh random priority, and so does every runnable task whose next
/// step conflicts with the one that just ran (see [`Task::next_step_conflicts_with`]). The relative
/// order of conflicting operations is thus resampled at every step, while that of operations that
/// commute is not. In the example above, POS runs the read after the write with probability
/// `1/(n+2)`.
///
/// What a task does next comes from [`Task::next_event`]. A scheduling point that doesn't say what
/// follows it has an [`EventKind::Unknown`] event, which conflicts with everything, so where Shuttle
/// knows nothing, POS resamples as a random walk would.
///
/// Strict priorities would starve a task behind one that never stops, such as a task that spins. So
/// a task that is runnable but doesn't run gains `decay` priority at every step. With the default
/// decay, a task that hasn't run for about 20,000 steps outranks every fresh priority. A task that
/// yields drops to the lowest priority, as with [`PctScheduler`](crate::PctScheduler).
///
/// The RNG is contained within the scheduler, so the scheduler can be reused across executions to
/// get different schedules each time. Two schedulers created from the same seed make the same
/// decisions when running the same test.
///
/// [`Task::next_step_conflicts_with`]: shuttle_engine::runtime::task::Task::next_step_conflicts_with
/// [`Task::next_event`]: shuttle_engine::runtime::task::Task::next_event
/// [`EventKind::Unknown`]: shuttle_engine::runtime::task::EventKind::Unknown
#[derive(Debug)]
pub struct PosScheduler {
    max_iterations: usize,
    iterations: usize,
    decay: u64,
    /// The priority of each task of the current execution that has been runnable, by task id.
    priorities: Vec<u64>,
    rng: Pcg64Mcg,
    data_source: RandomDataSource,
    current_seed: CurrentSeedDropGuard,
}

#[derive(Debug, Default)]
struct CurrentSeedDropGuard {
    inner: Option<u64>,
}

impl CurrentSeedDropGuard {
    fn clear(&mut self) {
        self.inner = None
    }

    fn update(&mut self, seed: u64) {
        self.inner = Some(seed)
    }
}

impl Drop for CurrentSeedDropGuard {
    fn drop(&mut self) {
        if let Some(s) = self.inner {
            eprintln!(
                "failing seed:\n\"\n{s}\n\"\nTo replay the failure, set the environment variable SHUTTLE_RANDOM_SEED to the seed and run the test under `shuttle::check_pos` (or another `PosScheduler` with the same decay)."
            )
        }
    }
}

impl PosScheduler {
    /// The default `decay`: a task gains this much priority at every step it is runnable but does
    /// not run, so after about 20,000 such steps it outranks every fresh priority, which is below
    /// `2^32`.
    pub const DEFAULT_DECAY: u64 = (1 << 32) / 20_000;

    /// Construct a new `PosScheduler` with a freshly seeded RNG and the default decay.
    pub fn new(max_iterations: usize) -> Self {
        Self::new_from_seed(OsRng.next_u64(), max_iterations)
    }

    /// Construct a new `PosScheduler` with a given seed and the default decay.
    ///
    /// Two `PosScheduler`s initialized with the same seed (and decay) will make the same scheduling
    /// decisions when executing the same workloads.
    ///
    /// If the `SHUTTLE_RANDOM_SEED` environment variable is set, then that seed will be used instead.
    pub fn new_from_seed(seed: u64, max_iterations: usize) -> Self {
        let seed = seed_from_env(seed);
        Self {
            max_iterations,
            iterations: 0,
            decay: Self::DEFAULT_DECAY,
            priorities: Vec::new(),
            rng: Pcg64Mcg::seed_from_u64(seed),
            data_source: RandomDataSource::initialize(seed),
            current_seed: CurrentSeedDropGuard::default(),
        }
    }

    /// Use `decay` instead of [`DEFAULT_DECAY`](Self::DEFAULT_DECAY): the priority that a runnable
    /// task that does not run gains at every step. Zero turns aging off, which can starve a task
    /// behind one that never stops running.
    pub fn with_decay(mut self, decay: u64) -> Self {
        self.decay = decay;
        self
    }

    /// A fresh priority, below `2^32`.
    fn fresh_priority(&mut self) -> u64 {
        u64::from(self.rng.next_u32())
    }

    /// The priority of `task`, which gets a fresh one the first time it is runnable.
    fn priority(&mut self, task: TaskId) -> u64 {
        let index = usize::from(task);
        while self.priorities.len() <= index {
            let priority = self.fresh_priority();
            self.priorities.push(priority);
        }
        self.priorities[index]
    }
}

impl Scheduler for PosScheduler {
    fn new_execution(&mut self) -> Option<Schedule> {
        if self.iterations >= self.max_iterations {
            self.current_seed.clear();
            None
        } else {
            self.iterations += 1;
            let seed = self.data_source.reinitialize();
            if let Ok(path) = std::env::var("SHUTTLE_ALWAYS_PERSIST_SEED") {
                std::fs::write(path, seed.to_string()).expect("Failed to write seed to file");
            }
            self.rng = Pcg64Mcg::seed_from_u64(seed);
            self.priorities.clear();
            self.current_seed.update(seed);
            Some(Schedule::new(seed))
        }
    }

    fn next_task(&mut self, runnable: &[&Task], current: Option<TaskId>, is_yielding: bool) -> Option<TaskId> {
        // A task that yields asks for the others to run first.
        if is_yielding && runnable.len() > 1 {
            if let Some(current) = current {
                self.priority(current);
                self.priorities[usize::from(current)] = 0;
            }
        }

        // The runnable task with the highest priority runs; ties go to the lowest task id.
        let mut chosen = runnable[0];
        let mut highest = self.priority(chosen.id());
        for &task in &runnable[1..] {
            let priority = self.priority(task.id());
            if priority > highest {
                chosen = task;
                highest = priority;
            }
        }

        // The chosen task, and every task whose next step conflicts with the chosen one's, get a
        // fresh priority. Every other runnable task ages.
        for &task in runnable {
            let index = usize::from(task.id());
            self.priorities[index] = if task.id() == chosen.id() || task.next_step_conflicts_with(chosen) {
                self.fresh_priority()
            } else {
                self.priorities[index].saturating_add(self.decay)
            };
        }

        Some(chosen.id())
    }

    fn next_u64(&mut self) -> u64 {
        self.data_source.next_u64()
    }
}
