use crate::runtime::task::TaskId;
use crate::scheduler::Scheduler;

/// A scheduler that performs an exhaustive, depth-first enumeration of all possible schedules.
#[derive(Debug)]
pub struct DFSScheduler {
    max_iterations: Option<usize>,
    max_depth: Option<usize>,
    iterations: usize,
    // Vec<(previous choice, was that the last choice at that level)>
    levels: Vec<(TaskId, bool)>,
    steps: usize,
}

impl DFSScheduler {
    /// Construct a new DFSScheduler with an optional bound on how many iterations to run
    /// and an optional bound on the maximum depth to explore.
    pub fn new(max_iterations: Option<usize>, max_depth: Option<usize>) -> Self {
        Self {
            max_iterations,
            max_depth,
            iterations: 0,
            levels: vec![],
            steps: 0,
        }
    }

    /// Check if there are any scheduling points at or below the `index`th level that have remaining
    /// schedulable tasks to explore.
    // TODO probably should memoize this -- at each iteration, just need to know the largest i
    // TODO such that levels[i].1 == true, which is the place we'll make a change
    fn has_more_choices(&self, index: usize) -> bool {
        self.levels[index..].iter().any(|(_, last)| !*last)
    }
}

impl Scheduler for DFSScheduler {
    fn new_execution(&mut self) -> bool {
        if self.max_iterations.map(|mi| self.iterations >= mi).unwrap_or(false) {
            return false;
        }

        // If there are no more choices to make at any level, we're done
        if self.iterations > 0 && !self.has_more_choices(0) {
            return false;
        }

        self.iterations += 1;
        self.steps = 0;
        true
    }

    fn next_task(&mut self, runnable: &[TaskId], _current: Option<TaskId>) -> Option<TaskId> {
        // TODO for this to work, the order of runnable needs to be deterministic. do we ensure that?
        // TODO should we remember more state so we can check if the test program is deterministic?
        if self.max_depth.map(|md| self.steps >= md).unwrap_or(false) {
            return None;
        }

        let next = if self.steps >= self.levels.len() {
            // First time we've reached this level
            assert_eq!(self.steps, self.levels.len());
            let to_run = runnable.first().unwrap();
            self.levels.push((*to_run, runnable.len() == 1));
            *to_run
        } else {
            let (last_choice, was_last) = self.levels[self.steps];
            if self.has_more_choices(self.steps + 1) {
                // Keep the same choice, because there's more work to do somewhere below us
                last_choice
            } else {
                // Time to make a change at this level
                assert!(
                    !was_last,
                    "if we are making a change, there should be another available option"
                );
                let next_idx = runnable.iter().position(|tid| *tid == last_choice).unwrap() + 1;
                let next = runnable[next_idx];
                self.levels.drain(self.steps..);
                self.levels.push((next, next_idx == runnable.len() - 1));
                next
            }
        };

        self.steps += 1;

        Some(next)
    }
}
