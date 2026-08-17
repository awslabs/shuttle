//! An incrementally maintained view of which tasks a scheduling decision needs
//! to consider.
//!
//! [`ExecutionState::schedule`](crate::runtime::execution::ExecutionState) used
//! to rebuild this information by walking the whole task list on every step.
//! Because tasks are never removed from that list, the walk was proportional to
//! the number of tasks the execution had *ever* created, so finished and blocked
//! tasks kept being re-examined forever. This type tracks the same facts by
//! reacting to task state transitions instead, which makes a scheduling decision
//! proportional to the number of *schedulable* tasks.
//!
//! Every field here is derivable from the `(TaskState, detached)` pair of each
//! live task, and `ExecutionState::schedule` re-derives all of it from scratch
//! and compares under `debug_assertions`, so a missed transition is a test
//! failure rather than a silent scheduling change.

use crate::runtime::task::{TaskId, TaskState};

const BITS: usize = usize::BITS as usize;

/// A dense bitset keyed by `TaskId`. Task ids are allocated densely from zero,
/// so this stays compact, and iteration can skip 64 empty slots at a time.
#[derive(Debug, Default, Clone)]
struct TaskIdSet {
    words: Vec<usize>,
}

impl TaskIdSet {
    #[inline]
    fn insert(&mut self, id: usize) {
        let (word, bit) = (id / BITS, id % BITS);
        if word >= self.words.len() {
            self.words.resize(word + 1, 0);
        }
        self.words[word] |= 1 << bit;
    }

    #[inline]
    fn remove(&mut self, id: usize) {
        let (word, bit) = (id / BITS, id % BITS);
        if let Some(w) = self.words.get_mut(word) {
            *w &= !(1 << bit);
        }
    }

    fn clear(&mut self) {
        // Zero in place rather than freeing, because the `verify-schedulable`
        // cross-check clears and refills a scratch set on every scheduling
        // decision and should not allocate to do it.
        self.words.iter_mut().for_each(|w| *w = 0);
    }

    /// Same members, ignoring any difference in trailing all-zero words.
    #[cfg(any(test, feature = "verify-schedulable"))]
    fn same_bits(&self, other: &Self) -> bool {
        let n = self.words.len().max(other.words.len());
        (0..n).all(|i| self.words.get(i).copied().unwrap_or(0) == other.words.get(i).copied().unwrap_or(0))
    }
}

/// Which tasks are eligible to be scheduled, and the aggregate facts about task
/// states that a scheduling decision needs.
#[derive(Debug, Default)]
pub(crate) struct SchedulableTasks {
    /// Tasks in [`TaskState::Runnable`].
    runnable: TaskIdSet,
    /// Tasks blocked but permitted to wake up spuriously. These are offered to
    /// the scheduler but do not count as runnable for deadlock detection.
    spurious: TaskIdSet,

    runnable_count: usize,
    /// Runnable tasks that are not detached. Zero means every runnable task is
    /// detached.
    runnable_attached_count: usize,
    unfinished_attached_count: usize,
    unfinished_detached_count: usize,
}

impl SchedulableTasks {
    /// Record a newly created task. Tasks are always born runnable and attached.
    pub(crate) fn register(&mut self, id: TaskId) {
        self.add(id, TaskState::Runnable, false);
    }

    /// Record a task's state transition. `detached` is the task's *current*
    /// detached flag, which this call does not change.
    pub(crate) fn transition(&mut self, id: TaskId, old: TaskState, new: TaskState, detached: bool) {
        if old == new {
            return;
        }
        self.remove(id, old, detached);
        self.add(id, new, detached);
    }

    /// Record that a task became detached. Detaching is one-way.
    pub(crate) fn detach(&mut self, id: TaskId, state: TaskState) {
        // Only the attached/detached tallies move; set membership is unaffected.
        self.remove(id, state, false);
        self.add(id, state, true);
    }

    fn add(&mut self, id: TaskId, state: TaskState, detached: bool) {
        let idx = id.0;
        match state {
            TaskState::Runnable => {
                self.runnable.insert(idx);
                self.runnable_count += 1;
                if !detached {
                    self.runnable_attached_count += 1;
                }
            }
            TaskState::Blocked {
                allow_spurious_wakeups: true,
            } => self.spurious.insert(idx),
            TaskState::Blocked {
                allow_spurious_wakeups: false,
            }
            | TaskState::Sleeping => {}
            TaskState::Finished => {}
        }
        if state != TaskState::Finished {
            if detached {
                self.unfinished_detached_count += 1;
            } else {
                self.unfinished_attached_count += 1;
            }
        }
    }

    fn remove(&mut self, id: TaskId, state: TaskState, detached: bool) {
        let idx = id.0;
        match state {
            TaskState::Runnable => {
                self.runnable.remove(idx);
                self.runnable_count -= 1;
                if !detached {
                    self.runnable_attached_count -= 1;
                }
            }
            TaskState::Blocked {
                allow_spurious_wakeups: true,
            } => self.spurious.remove(idx),
            TaskState::Blocked {
                allow_spurious_wakeups: false,
            }
            | TaskState::Sleeping => {}
            TaskState::Finished => {}
        }
        if state != TaskState::Finished {
            if detached {
                self.unfinished_detached_count -= 1;
            } else {
                self.unfinished_attached_count -= 1;
            }
        }
    }

    /// Is any task runnable? Tasks that are merely eligible for a spurious
    /// wakeup do not count, because there is no guarantee such a wakeup ever
    /// happens, so an execution with only those left is a deadlock.
    pub(crate) fn any_runnable(&self) -> bool {
        self.runnable_count > 0
    }

    /// Are all runnable tasks detached?
    pub(crate) fn all_runnable_detached(&self) -> bool {
        self.runnable_attached_count == 0
    }

    pub(crate) fn has_unfinished_attached(&self) -> bool {
        self.unfinished_attached_count > 0
    }

    pub(crate) fn unfinished_attached_count(&self) -> usize {
        self.unfinished_attached_count
    }

    pub(crate) fn has_unfinished_detached(&self) -> bool {
        self.unfinished_detached_count > 0
    }

    /// Visit every task the scheduler may pick, in ascending `TaskId` order.
    ///
    /// Order matters: schedulers index into the slice built from this, so
    /// changing the order would change which task a given random choice selects
    /// and invalidate recorded schedules.
    pub(crate) fn for_each<F: FnMut(TaskId)>(&self, mut f: F) {
        let len = self.runnable.words.len().max(self.spurious.words.len());
        for word_idx in 0..len {
            let runnable = self.runnable.words.get(word_idx).copied().unwrap_or(0);
            let spurious = self.spurious.words.get(word_idx).copied().unwrap_or(0);
            let mut bits = runnable | spurious;
            while bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                f(TaskId(word_idx * BITS + bit));
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        self.runnable.clear();
        self.spurious.clear();
        self.runnable_count = 0;
        self.runnable_attached_count = 0;
        self.unfinished_attached_count = 0;
        self.unfinished_detached_count = 0;
    }

    /// Rebuild `self` from the authoritative task list, reusing existing
    /// allocations. Used by the `verify-schedulable` cross-check.
    #[cfg(any(test, feature = "verify-schedulable"))]
    pub(crate) fn recompute_from<I>(&mut self, tasks: I)
    where
        I: IntoIterator<Item = (TaskId, TaskState, bool)>,
    {
        self.clear();
        for (id, state, detached) in tasks {
            self.add(id, state, detached);
        }
    }

    /// Structural equality, for the `verify-schedulable` cross-check.
    #[cfg(any(test, feature = "verify-schedulable"))]
    pub(crate) fn matches(&self, other: &Self) -> bool {
        self.runnable_count == other.runnable_count
            && self.runnable_attached_count == other.runnable_attached_count
            && self.unfinished_attached_count == other.unfinished_attached_count
            && self.unfinished_detached_count == other.unfinished_detached_count
            && self.runnable.same_bits(&other.runnable)
            && self.spurious.same_bits(&other.spurious)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCKED_SPURIOUS: TaskState = TaskState::Blocked {
        allow_spurious_wakeups: true,
    };
    const BLOCKED: TaskState = TaskState::Blocked {
        allow_spurious_wakeups: false,
    };

    fn ids(s: &SchedulableTasks) -> Vec<usize> {
        let mut v = Vec::new();
        s.for_each(|id| v.push(id.0));
        v
    }

    #[test]
    fn ascending_order_across_word_boundaries() {
        let mut s = SchedulableTasks::default();
        // Deliberately register out of order and across several 64-bit words.
        for id in [200usize, 3, 64, 65, 0, 127, 128] {
            s.register(TaskId(id));
        }
        assert_eq!(ids(&s), vec![0, 3, 64, 65, 127, 128, 200]);
    }

    #[test]
    fn spurious_tasks_are_offered_but_are_not_runnable() {
        let mut s = SchedulableTasks::default();
        s.register(TaskId(0));
        s.register(TaskId(1));

        s.transition(TaskId(1), TaskState::Runnable, BLOCKED_SPURIOUS, false);
        assert_eq!(ids(&s), vec![0, 1], "spurious task is still offered");
        assert!(s.any_runnable());

        s.transition(TaskId(0), TaskState::Runnable, BLOCKED_SPURIOUS, false);
        assert_eq!(ids(&s), vec![0, 1]);
        assert!(!s.any_runnable(), "only spurious tasks left is a deadlock");
        assert!(s.has_unfinished_attached());
    }

    #[test]
    fn plain_blocked_and_sleeping_tasks_are_not_offered() {
        let mut s = SchedulableTasks::default();
        s.register(TaskId(0));
        s.register(TaskId(1));

        s.transition(TaskId(0), TaskState::Runnable, BLOCKED, false);
        s.transition(TaskId(1), TaskState::Runnable, TaskState::Sleeping, false);
        assert!(ids(&s).is_empty());
        assert!(!s.any_runnable());
        assert_eq!(s.unfinished_attached_count(), 2);
    }

    #[test]
    fn finishing_clears_membership_and_tallies() {
        let mut s = SchedulableTasks::default();
        s.register(TaskId(0));
        s.transition(TaskId(0), TaskState::Runnable, TaskState::Finished, false);
        assert!(ids(&s).is_empty());
        assert!(!s.any_runnable());
        assert!(!s.has_unfinished_attached());
        assert!(!s.has_unfinished_detached());
    }

    #[test]
    fn detached_tasks_tracked_separately() {
        let mut s = SchedulableTasks::default();
        s.register(TaskId(0));
        s.register(TaskId(1));
        assert!(!s.all_runnable_detached());

        s.detach(TaskId(1), TaskState::Runnable);
        assert!(s.has_unfinished_detached());
        assert!(!s.all_runnable_detached(), "task 0 is still attached");

        s.detach(TaskId(0), TaskState::Runnable);
        assert!(s.all_runnable_detached());
        assert!(!s.has_unfinished_attached());
    }

    #[test]
    fn transition_between_blocked_variants() {
        let mut s = SchedulableTasks::default();
        s.register(TaskId(0));

        s.transition(TaskId(0), TaskState::Runnable, BLOCKED, false);
        assert!(ids(&s).is_empty());

        s.transition(TaskId(0), BLOCKED, BLOCKED_SPURIOUS, false);
        assert_eq!(ids(&s), vec![0]);

        s.transition(TaskId(0), BLOCKED_SPURIOUS, BLOCKED, false);
        assert!(ids(&s).is_empty());
        assert_eq!(s.unfinished_attached_count(), 1);
    }

    #[test]
    fn clear_resets_everything() {
        let mut s = SchedulableTasks::default();
        s.register(TaskId(0));
        s.register(TaskId(70));
        s.transition(TaskId(70), TaskState::Runnable, BLOCKED_SPURIOUS, false);
        s.clear();

        assert!(ids(&s).is_empty());
        assert!(!s.any_runnable());
        assert!(!s.has_unfinished_attached());
        assert!(!s.has_unfinished_detached());
        assert!(s.all_runnable_detached());

        // Reusable after clearing, with no residue from before.
        s.register(TaskId(1));
        assert_eq!(ids(&s), vec![1]);
        assert_eq!(s.unfinished_attached_count(), 1);
    }

    #[test]
    fn repeated_no_op_transitions_do_not_drift() {
        let mut s = SchedulableTasks::default();
        s.register(TaskId(0));
        for _ in 0..5 {
            s.transition(TaskId(0), TaskState::Runnable, TaskState::Runnable, false);
        }
        assert_eq!(ids(&s), vec![0]);
        assert_eq!(s.unfinished_attached_count(), 1);
    }

    #[test]
    fn matches_a_full_recomputation() {
        let mut s = SchedulableTasks::default();
        for id in 0..5 {
            s.register(TaskId(id));
        }
        s.transition(TaskId(1), TaskState::Runnable, BLOCKED_SPURIOUS, false);
        s.transition(TaskId(2), TaskState::Runnable, TaskState::Sleeping, false);
        s.transition(TaskId(3), TaskState::Runnable, TaskState::Finished, false);
        s.detach(TaskId(4), TaskState::Runnable);

        let mut expected = SchedulableTasks::default();
        expected.recompute_from(vec![
            (TaskId(0), TaskState::Runnable, false),
            (TaskId(1), BLOCKED_SPURIOUS, false),
            (TaskId(2), TaskState::Sleeping, false),
            (TaskId(3), TaskState::Finished, false),
            (TaskId(4), TaskState::Runnable, true),
        ]);
        assert!(s.matches(&expected));
    }
}
