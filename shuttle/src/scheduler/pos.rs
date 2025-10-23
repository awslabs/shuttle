use crate::runtime::task::Event;
use crate::runtime::task::{Task, TaskId, TaskSignature};
use crate::scheduler::data::random::RandomDataSource;
use crate::scheduler::data::DataSource;
use crate::scheduler::{Schedule, Scheduler};
use crate::sync::ResourceSignature;
use rand::rngs::OsRng;
use rand::{RngCore, SeedableRng};
use rand_pcg::Pcg64Mcg;

/// A scheduler that implements the Partial Order Sampling (POS) algorithm.
///
/// The POS algorithm comes from the paper "Partial Order Aware Concurrency Sampling", Yuan et al,
/// CAV 2018. This scheduler assign each task a random priority, always choosing the task with the
/// *highest* priority to run next. Priorities are reset either when (1) the task is run or (2) a
/// task whose next event interferes with this task's next event is run. Priorities also decay over
/// time to avoid tasks being stuck with extremely low priorities for long portions of the execution.
///
/// The RNG used is contained within the scheduler, allowing it to be reused across executions in
/// order to get different random schedules each time. The scheduler has an optional bias towards
/// remaining on the current task.
#[derive(Debug)]
pub struct PosScheduler {
    decay: u32,
    priorities: Vec<u32>,
    max_iterations: usize,
    rng: Pcg64Mcg,
    iterations: usize,
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
                "failing seed:\n\"\n{s}\n\"\nTo replay the failure, either:\n    1) pass the seed to `shuttle::check_random_with_seed, or\n    2) set the environment variable SHUTTLE_RANDOM_SEED to the seed and run `shuttle::check_random`."
            )
        }
    }
}

impl PosScheduler {
    /// Construct a new PosScheduler with a freshly seeded RNG and default decay.
    pub fn new(max_iterations: usize) -> Self {
        // Decay to ~10k steps in expectation
        Self::new_from_seed(OsRng.next_u64(), max_iterations, 214748)
    }

    /// Construct a new PosScheduler with a given seed and decay.
    ///
    /// Two PosSchedulers initialized with the same seed will make the same scheduling decisions
    /// when executing the same workloads.
    ///
    /// If the `SHUTTLE_RANDOM_SEED` environment variable is set, then that seed will be used instead.
    pub fn new_from_seed(seed: u64, max_iterations: usize, decay: u32) -> Self {
        let seed_env = std::env::var("SHUTTLE_RANDOM_SEED");
        let seed = match seed_env {
            Ok(s) => match s.as_str().parse::<u64>() {
                Ok(seed) => {
                    tracing::info!(
                        "Initializing PosScheduler with the seed provided by SHUTTLE_RANDOM_SEED: {}",
                        seed
                    );
                    seed
                }
                Err(err) => panic!("The seed provided by SHUTTLE_RANDOM_SEED is not a valid u64: {err}"),
            },
            Err(_) => seed,
        };

        let rng = Pcg64Mcg::seed_from_u64(seed);

        Self {
            decay,
            priorities: Vec::new(),
            max_iterations,
            rng,
            iterations: 0,
            data_source: RandomDataSource::initialize(seed),
            current_seed: CurrentSeedDropGuard::default(),
        }
    }
}

impl Scheduler for PosScheduler {
    fn new_execution(&mut self) -> Option<Schedule> {
        if self.iterations >= self.max_iterations {
            self.current_seed.clear();
            self.priorities.clear();
            None
        } else {
            self.iterations += 1;
            let seed = self.data_source.reinitialize();
            self.priorities.clear();
            self.rng = Pcg64Mcg::seed_from_u64(seed);
            self.current_seed.update(seed);
            Some(Schedule::new(seed))
        }
    }

    fn next_task<'a>(
        &mut self,
        runnable: &'a [&'a Task],
        _current: Option<TaskId>,
        _is_yielding: bool,
    ) -> Option<&'a Task> {
        let mut max_priority: i64 = -1;
        let mut max_priority_task = None;
        for t in runnable {
            // New task
            while get_tid(t) >= self.priorities.len() {
                self.priorities.push(self.rng.next_u32());
            }

            let priority = *self.priorities.get(get_tid(t)).unwrap() as i64;
            if priority > max_priority {
                max_priority = priority;
                max_priority_task = Some(*t);
            }
        }

        if let Some(next_task) = max_priority_task {
            for t in runnable {
                if t.id() == next_task.id() || is_racing(t.next_event(), next_task.next_event()) {
                    self.priorities[get_tid(t)] = self.rng.next_u32();
                } else {
                    // Decay to avoid tasks with low priority never being run
                    self.priorities[get_tid(t)] = self.priorities[get_tid(t)].saturating_add(self.decay);
                }
            }
        }

        max_priority_task
    }

    fn next_u64(&mut self) -> u64 {
        self.data_source.next_u64()
    }
}

#[inline]
fn get_tid(task: &Task) -> usize {
    task.id().into()
}

fn resource_id(e: &Event) -> Option<usize> {
    // Since POS doesn't care about stability across executions, using the address of the signature is more precise than the signature itself for const constructed resources
    match e {
        Event::AtomicRead(s, _) |
        Event::AtomicWrite(s, _) |
        Event::AtomicReadWrite(s, _) |
        Event::BatchSemaphoreAcq(s, _) |
        Event::BatchSemaphoreRel(s, _) |
        Event::BarrierWait(s, _) |
        Event::ChannelSend(s, _) |
        Event::ChannelRecv(s, _) |
        Event::CondvarWait(s, _) => Some((*s as *const ResourceSignature) as usize),
        Event::Unpark(s, _) |
        Event::Join(s, _) |
        Event::Spawn(s) => Some((*s as *const TaskSignature) as usize),
        Event::CondvarNotify(_) |
        Event::Park(_) | // TODO, add the TaskSignature for Park
        Event::Yield(_) |
        Event::Sleep(_) |
        Event::Exit |
        Event::Unknown => None,
    }
}

fn is_write(e: &Event) -> bool {
    matches!(
        e,
        Event::AtomicWrite(_, _)
            | Event::AtomicReadWrite(_, _)
            | Event::BatchSemaphoreAcq(_, _)
            | Event::BatchSemaphoreRel(_, _)
            | Event::BarrierWait(_, _)
            | Event::ChannelSend(_, _)
            | Event::ChannelRecv(_, _)
            | Event::CondvarWait(_, _)
            | Event::Unpark(_, _)
            | Event::CondvarNotify(_)
    )
}

fn is_racing(e1: &Event, e2: &Event) -> bool {
    resource_id(e1) == resource_id(e2) && (is_write(e1) || is_write(e2))
}
