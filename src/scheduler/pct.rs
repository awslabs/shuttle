use crate::runtime::task::{TaskId, MAX_TASKS};
use crate::scheduler::data::random::RandomDataSource;
use crate::scheduler::data::DataSource;
use crate::scheduler::{Schedule, Scheduler};
use rand::rngs::OsRng;
use rand::seq::SliceRandom;
use rand::{Rng, RngCore, SeedableRng};
use rand_pcg::Pcg64Mcg;

/// A scheduler that implements the Probabilistic Concurrency Testing (PCT) algorithm.
///
/// The PCT algorithm comes from the paper "A Randomized Scheduler with Probabilistic Guarantees of
/// Finding Bugs", Burckhardt et al, ASPLOS 2010. This implementation follows the one in Coyote
/// (https://github.com/microsoft/coyote/blob/master/Source/Core/SystematicTesting/Strategies/Probabilistic/PCTStrategy.cs)
/// which differs slightly from the paper (see notes in `next_task`) and supports dynamically
/// determining the bound on the number of steps.
#[derive(Debug)]
pub struct PCTScheduler {
    max_iterations: usize,
    max_depth: usize,
    iterations: usize,
    // invariant: always contains all possible TaskIds (never added or removed)
    priority_queue: Vec<TaskId>,
    // invariant: length is self.max_depth - 1
    change_points: Vec<usize>,
    max_steps: usize,
    steps: usize,
    rng: Pcg64Mcg,
    data_source: RandomDataSource,
}

impl PCTScheduler {
    /// Construct a new PCTScheduler with a freshly seeded RNG.
    pub fn new(max_depth: usize, max_iterations: usize) -> Self {
        Self::new_from_seed(OsRng.next_u64(), max_depth, max_iterations)
    }

    /// Construct a new PCTScheduler with a given seed.
    pub fn new_from_seed(seed: u64, max_depth: usize, max_iterations: usize) -> Self {
        assert!(max_depth > 0);

        let rng = Pcg64Mcg::seed_from_u64(seed);

        Self {
            max_iterations,
            max_depth,
            iterations: 0,
            priority_queue: (0..MAX_TASKS).map(TaskId::from).collect::<Vec<_>>(),
            change_points: vec![],
            max_steps: 0,
            steps: 0,
            rng,
            data_source: RandomDataSource::initialize(seed),
        }
    }
}

impl Scheduler for PCTScheduler {
    fn new_execution(&mut self) -> Option<Schedule> {
        if self.iterations >= self.max_iterations {
            return None;
        }

        self.steps = 0;

        // On the first iteration, we run a simple oldest-task-first scheduler to determine a
        // bound on the maximum number of steps. Once we have that, we can initialize PCT.
        if self.iterations > 0 {
            // Initialize priorities by shuffling the task IDs
            self.priority_queue.shuffle(&mut self.rng);

            // Initialize change points by sampling from the current max_steps. We skip step 0
            // because there's no point making a priority change before any tasks have run; the
            // random priority initialization takes care of that.
            if self.change_points.len() != self.max_depth - 1 {
                self.change_points = vec![usize::MAX, self.max_depth - 1];
            }
            let rng = &mut self.rng;
            let max_steps = self.max_steps;
            if max_steps > 1 {
                for point in self.change_points.iter_mut() {
                    *point = rng.gen_range(1, max_steps);
                }
            }
        }

        self.iterations += 1;

        Some(Schedule::new(self.data_source.reinitialize()))
    }

    fn next_task(&mut self, runnable: &[TaskId], current: Option<TaskId>) -> Option<TaskId> {
        // No point doing priority changes when there's only one runnable task. This also means that
        // our step counter is counting actual scheduling decisions, not no-ops where there was no
        // choice about which task to run. From the paper (4.1, "Identifying Sequential Execution"):
        // > Inserting priority change points during sequential execution is not necessary. The same
        // > effect can be achieved by reducing the priority at the point the sequential thread
        // > enables/creates a second thread.
        // TODO is this really correct? need to think about it more
        if runnable.len() > 1 {
            if self.change_points.contains(&self.steps) {
                // Deprioritize `current` by moving it to the end of the list
                // TODO in the paper, the i'th change point gets priority i, whereas this gives d-i.
                // TODO I don't think this matters, because the change points are randomized.
                let current = current.expect("self.steps > 0 should mean a task has run");
                let idx = self.priority_queue.iter().position(|tid| *tid == current).unwrap();
                self.priority_queue.remove(idx);
                self.priority_queue.push(current);
            }

            self.steps += 1;
            if self.steps > self.max_steps {
                self.max_steps = self.steps;
            }
        }

        // Choose the highest-priority (== earliest in the queue) runnable task
        Some(*self.priority_queue.iter().find(|tid| runnable.contains(tid)).unwrap())
    }

    fn next_u64(&mut self) -> u64 {
        self.data_source.next_u64()
    }
}
