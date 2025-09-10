use crate::runtime::task::{Task, TaskId};
use crate::scheduler::data::random::RandomDataSource;
use crate::scheduler::data::DataSource;
use crate::scheduler::{Schedule, Scheduler};
use rand::rngs::OsRng;
use rand::seq::SliceRandom;
use rand::{RngCore, SeedableRng};
use rand_pcg::Pcg64Mcg;
use std::collections::{HashMap, HashSet};
use tracing::{info, trace, warn};

/// A scheduler which implements Uniform Random Walk (URW) from "Selectively Uniform Concurrency Testing" by Huan Zhao,
/// Dylan Wolff, Umang Mathur, and Abhik Roychoudhury - [ASPLOS '25](https://dl.acm.org/doi/abs/10.1145/3669940.3707214).
/// The URW algorithm samples all interleavings *uniformly* given an accurate estimate of the number of events which will
/// take place on each task. This implementation uses a single trial run of the program to generate these estimates.
/// During the trial run, it uses vanilla random walk for scheduling (identical to [`crate::scheduler::RandomScheduler`]).
/// As discussed in the paper, the event count for a task is equal to the number of scheduling points remaining on that
/// task added to the sum of event counts over each of the task's yet-to-be-spawned children.
///
/// The RNG used is contained within the scheduler, allowing it to be reused across executions in order to get different
/// random schedules each time
#[derive(Debug)]
pub struct UniformRandomScheduler {
    max_iterations: usize,
    rng: Pcg64Mcg,
    iterations: usize,
    data_source: RandomDataSource,
    /// Number of events remaining on each task in the current execution
    task_event_counts: Option<Vec<usize>>,
    /// Event count estimates for each task by signature
    signature_event_counts: HashMap<u64, usize>,
    /// Indicates a parent-child relation between the task with signature .0 and .1
    signature_parents: Vec<(u64, u64)>,
}

impl UniformRandomScheduler {
    /// Construct a new UniformRandomScheduler with a freshly seeded RNG.
    pub fn new(max_iterations: usize) -> Self {
        Self::new_from_seed(OsRng.next_u64(), max_iterations)
    }

    /// Construct a UniformRandomScheduler with a given seed.
    /// Two UniformRandomSchedulers initialized with the same seed will make the same scheduling decisions when executing the same workloads.
    /// If the `SHUTTLE_RANDOM_SEED` environment variable is set, then that seed will be used instead.
    pub fn new_from_seed(seed: u64, max_iterations: usize) -> Self {
        let seed_env = std::env::var("SHUTTLE_RANDOM_SEED");
        let seed = match seed_env {
            Ok(s) => match s.as_str().parse::<u64>() {
                Ok(seed) => {
                    tracing::info!(
                        "Initializing UniformRandomScheduler with the seed provided by SHUTTLE_RANDOM_SEED: {}",
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
            max_iterations,
            rng,
            iterations: 0,
            data_source: RandomDataSource::initialize(seed),
            task_event_counts: None,
            signature_event_counts: HashMap::new(),
            signature_parents: Vec::new(),
        }
    }

    #[inline]
    fn events_counted_but_not_initialized(&self) -> bool {
        self.task_event_counts.is_none() && !self.signature_event_counts.is_empty()
    }
}

impl Scheduler for UniformRandomScheduler {
    fn new_execution(&mut self) -> Option<Schedule> {
        if self.iterations >= self.max_iterations {
            self.signature_event_counts.clear();
            self.signature_parents.clear();
            self.task_event_counts = None;
            None
        } else {
            if self.events_counted_but_not_initialized() {
                // If we have never initialized the event counts before, we need to aggregate the count estimates
                // from each child task to its parents
                info!("Finished estimation of event counts for URW");
                info!(
                    "Estimated event counts for URW (pre-parent subsumption): {:?}",
                    self.signature_event_counts
                );
                // Iterating in reverse spawn order to ensure counts are propagated to grandparents correctly
                for (parent_sig, child_sig) in self.signature_parents.iter().rev() {
                    let child_ct = *self.signature_event_counts.get(child_sig).unwrap();
                    self.signature_event_counts
                        .entry(*parent_sig)
                        .and_modify(|parent_ct| *parent_ct += child_ct);
                }
                info!(
                    "Estimated event counts for URW (post-parent subsumption): {:?}",
                    self.signature_event_counts
                );

                // The implemenation of URW currently assumes that each task has a unique signature in a single execution.
                // Thus the number of spawns events should equal the number of unique child signatures
                debug_assert!(
                    self.signature_event_counts
                        .keys()
                        .cloned()
                        .collect::<HashSet<_>>()
                        .len()
                        == self.signature_event_counts.len()
                );

                self.task_event_counts = Some(Vec::new());
            } else if let Some(ec) = self.task_event_counts.as_mut() {
                // Clear any remaining event counts for the next iteration. Counts are re-initialized
                // on `spawn` in the next iteration
                ec.clear();
            }
            self.iterations += 1;
            let seed = self.data_source.reinitialize();
            self.rng = Pcg64Mcg::seed_from_u64(seed);
            Some(Schedule::new(seed))
        }
    }

    fn next_task(&mut self, runnable: &[&Task], _current: Option<TaskId>, _is_yielding: bool) -> Option<TaskId> {
        if let Some(task_event_counts) = self.task_event_counts.as_mut() {
            // If the event counts have been estimated...

            // We need to loop over the tasks to identify if there has been a `spawn` event
            // In the future, if we embed a `next_event` enum field into the Task struct, this code can be
            // simplified to a single conditional
            for t in runnable {
                let tid: usize = get_tid(t);
                if tid == task_event_counts.len() {
                    // Spawn: should remove child events from the parent and map remaining events to the correct task id
                    let child_events = *self
                        .signature_event_counts
                        .get(&t.signature.signature_hash())
                        .unwrap_or_else(|| {
                            // TODO: we can probably handle unseen tasks less naively than estimating a single event
                            warn!(
                                "No event count for spawn of task with signature {}",
                                t.signature.signature_hash()
                            );
                            &1
                        });
                    task_event_counts.push(child_events);
                    if let Some(ptid) = t.parent_task_id() {
                        let ptid: usize = ptid.into();
                        task_event_counts[ptid] = task_event_counts[ptid].saturating_sub(child_events).max(1);
                    }
                } else if tid > task_event_counts.len() {
                    panic!("TID's expected to be spawned in ascending order in increments of 1");
                }
                // Any runnable task must have at least one remaining event
                assert!(task_event_counts[tid] >= 1);
            }

            let next_tid = runnable
                .choose_weighted(&mut self.rng, |t| task_event_counts[get_tid(t)])
                .unwrap()
                .id();
            let next_tid_usize: usize = next_tid.into();
            task_event_counts[next_tid_usize] = task_event_counts[next_tid_usize].saturating_sub(1).max(1);

            trace!("URW remaining event counts: {:?}", task_event_counts);
            Some(next_tid)
        } else {
            // Delegate scheduling to vanilla RW when estimating counts
            let t = runnable.choose(&mut self.rng).unwrap();

            // If we don't have event counts yet, use the current run to estimate event counts (1-shot)
            self.signature_event_counts
                .entry(t.signature.signature_hash())
                .and_modify(|c| *c += 1)
                .or_insert_with(|| {
                    // Spawn
                    self.signature_parents
                        .push((t.signature.parent_signature_hash(), t.signature.signature_hash()));
                    1
                });

            Some(t.id())
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.data_source.next_u64()
    }
}

#[inline]
fn get_tid(task: &Task) -> usize {
    task.id().into()
}
