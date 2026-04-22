use super::{TaskId, TaskState, DEFAULT_INLINE_TASKS};
use smallvec::SmallVec;

/// `TaskManager` maintains a persistent, incrementally-updated view of which tasks are
/// runnable, avoiding the need to rescan all tasks on every scheduling decision.
///
/// It sits between `ExecutionState` and the `Scheduler`, tracking task state transitions
/// as they happen and providing the runnable set on demand.
#[derive(Debug)]
pub(crate) struct TaskManager {
    /// Per-task tracking info, indexed by `TaskId.0`
    entries: SmallVec<[TaskEntry; DEFAULT_INLINE_TASKS]>,

    /// IDs of tasks currently in the runnable set (includes spurious-wakeup candidates)
    runnable: SmallVec<[TaskId; DEFAULT_INLINE_TASKS]>,

    // Cached aggregate metadata, updated incrementally
    /// True if any task is in `TaskState::Runnable`
    any_truly_runnable: bool,
    /// True if there exists an unfinished, non-detached task
    unfinished_attached: bool,
    /// True if all truly-runnable tasks are detached
    all_runnable_detached: bool,
}

#[derive(Debug, Clone)]
struct TaskEntry {
    state: TaskState,
    detached: bool,
    /// Whether this task is currently in the `runnable` vec
    in_runnable_set: bool,
}

impl TaskManager {
    pub(crate) fn new() -> Self {
        Self {
            entries: SmallVec::new(),
            runnable: SmallVec::new(),
            any_truly_runnable: false,
            unfinished_attached: false,
            all_runnable_detached: true,
        }
    }

    /// Register a newly spawned task. New tasks always start as `Runnable`.
    pub(crate) fn task_spawned(&mut self, id: TaskId, detached: bool) {
        debug_assert_eq!(id.0, self.entries.len(), "tasks must be registered in order");
        self.entries.push(TaskEntry {
            state: TaskState::Runnable,
            detached,
            in_runnable_set: true,
        });
        self.runnable.push(id);
        // No need to sort — spawned IDs are monotonically increasing
        self.recompute_aggregates();
    }

    /// Notify that a task has transitioned to `Blocked`.
    pub(crate) fn task_blocked(&mut self, id: TaskId, allow_spurious_wakeups: bool) {
        let entry = &mut self.entries[id.0];
        entry.state = TaskState::Blocked { allow_spurious_wakeups };

        if allow_spurious_wakeups {
            // Keep in runnable set (scheduler can pick it for spurious wakeup)
            if !entry.in_runnable_set {
                entry.in_runnable_set = true;
                self.runnable.push(id);
                self.sort_runnable();
            }
        } else {
            // Remove from runnable set
            self.remove_from_runnable(id);
        }
        self.recompute_aggregates();
    }

    /// Notify that a task has transitioned to `Runnable` (unblocked).
    /// This is idempotent — calling it on an already-runnable task is a no-op.
    pub(crate) fn task_unblocked(&mut self, id: TaskId) {
        let entry = &mut self.entries[id.0];
        if entry.state == TaskState::Runnable {
            // Already runnable, nothing to do
            return;
        }
        entry.state = TaskState::Runnable;
        if !entry.in_runnable_set {
            entry.in_runnable_set = true;
            self.runnable.push(id);
            self.sort_runnable();
        }
        self.recompute_aggregates();
    }

    /// Notify that a task has transitioned to `Sleeping`.
    pub(crate) fn task_sleeping(&mut self, id: TaskId) {
        let entry = &mut self.entries[id.0];
        entry.state = TaskState::Sleeping;
        self.remove_from_runnable(id);
        self.recompute_aggregates();
    }

    /// Notify that a task has finished.
    pub(crate) fn task_finished(&mut self, id: TaskId) {
        let entry = &mut self.entries[id.0];
        entry.state = TaskState::Finished;
        self.remove_from_runnable(id);
        self.recompute_aggregates();
    }

    /// Notify that a task has been detached.
    pub(crate) fn task_detached(&mut self, id: TaskId) {
        self.entries[id.0].detached = true;
        self.recompute_aggregates();
    }

    /// Returns whether execution should finish:
    /// (1) No truly runnable tasks, or
    /// (2) All runnable tasks are detached AND no unfinished attached tasks remain
    pub(crate) fn should_finish(&self) -> bool {
        !self.any_truly_runnable || (!self.unfinished_attached && self.all_runnable_detached)
    }

    /// Returns the current set of runnable task IDs (including spurious-wakeup candidates).
    pub(crate) fn runnable_tasks(&self) -> &[TaskId] {
        &self.runnable
    }

    fn remove_from_runnable(&mut self, id: TaskId) {
        let entry = &mut self.entries[id.0];
        if entry.in_runnable_set {
            entry.in_runnable_set = false;
            self.runnable.retain(|tid| *tid != id);
        }
    }

    /// Ensure the runnable set is sorted by TaskId.
    /// This maintains the same ordering as the original full-scan approach,
    /// which is important for deterministic schedulers like DFS.
    fn sort_runnable(&mut self) {
        self.runnable.sort_unstable();
    }

    /// Recompute the aggregate flags from scratch.
    ///
    /// This is O(n) in the number of tasks, but the number of tasks is typically small
    /// (< 16) and this avoids subtle bugs from trying to maintain these flags incrementally
    /// through complex state transitions.
    fn recompute_aggregates(&mut self) {
        self.any_truly_runnable = false;
        self.unfinished_attached = false;
        self.all_runnable_detached = true;

        for entry in &self.entries {
            if entry.state == TaskState::Finished {
                continue;
            }
            if !entry.detached {
                self.unfinished_attached = true;
            }
            if entry.state == TaskState::Runnable {
                self.any_truly_runnable = true;
                if !entry.detached {
                    self.all_runnable_detached = false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_spawn_and_finish() {
        let mut tm = TaskManager::new();
        tm.task_spawned(TaskId(0), false);
        assert!(!tm.should_finish());
        assert_eq!(tm.runnable_tasks(), &[TaskId(0)]);

        tm.task_finished(TaskId(0));
        assert!(tm.should_finish());
        assert!(tm.runnable_tasks().is_empty());
    }

    #[test]
    fn test_block_unblock() {
        let mut tm = TaskManager::new();
        tm.task_spawned(TaskId(0), false);
        tm.task_spawned(TaskId(1), false);

        tm.task_blocked(TaskId(0), false);
        assert_eq!(tm.runnable_tasks(), &[TaskId(1)]);
        assert!(!tm.should_finish());

        tm.task_unblocked(TaskId(0));
        assert_eq!(tm.runnable_tasks().len(), 2);
        assert!(!tm.should_finish());
    }

    #[test]
    fn test_spurious_wakeup_stays_in_runnable() {
        let mut tm = TaskManager::new();
        tm.task_spawned(TaskId(0), false);
        tm.task_spawned(TaskId(1), false);

        // Block with spurious wakeups allowed — should stay in runnable set
        tm.task_blocked(TaskId(0), true);
        assert_eq!(tm.runnable_tasks().len(), 2);
        // But should_finish should be false because task 1 is truly runnable
        assert!(!tm.should_finish());
    }

    #[test]
    fn test_only_spurious_wakeup_tasks_means_deadlock() {
        let mut tm = TaskManager::new();
        tm.task_spawned(TaskId(0), false);

        // Only task is blocked with spurious wakeup — not truly runnable
        tm.task_blocked(TaskId(0), true);
        assert!(tm.should_finish()); // should be treated as deadlock
    }

    #[test]
    fn test_detached_tasks() {
        let mut tm = TaskManager::new();
        tm.task_spawned(TaskId(0), false); // attached
        tm.task_spawned(TaskId(1), true);  // detached

        tm.task_finished(TaskId(0));
        // Only detached task remains runnable, no unfinished attached
        assert!(tm.should_finish());
    }

    #[test]
    fn test_detach_after_spawn() {
        let mut tm = TaskManager::new();
        tm.task_spawned(TaskId(0), false);
        tm.task_spawned(TaskId(1), false);

        tm.task_detached(TaskId(1));
        tm.task_finished(TaskId(0));
        assert!(tm.should_finish());
    }

    #[test]
    fn test_sleeping_and_wake() {
        let mut tm = TaskManager::new();
        tm.task_spawned(TaskId(0), false);

        tm.task_sleeping(TaskId(0));
        assert!(tm.runnable_tasks().is_empty());
        assert!(tm.should_finish());

        tm.task_unblocked(TaskId(0));
        assert_eq!(tm.runnable_tasks(), &[TaskId(0)]);
        assert!(!tm.should_finish());
    }
}
