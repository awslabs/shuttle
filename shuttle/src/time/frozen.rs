use std::{cmp::Reverse, collections::HashSet, task::Waker};

use tracing::warn;

use crate::{
    current::{with_labels_for_task, Labels, TaskId},
    runtime::execution::ExecutionState,
};

use super::{constant_stepped::ConstantSteppedTimeModel, Duration, Instant, TimeModel};

/// A time model where time does not advance unless forced
pub struct FrozenTimeModel {
    inner: ConstantSteppedTimeModel,
    expired: HashSet<TaskId>,
    #[allow(clippy::type_complexity)]
    triggers: Vec<Box<dyn Fn(&Labels) -> bool>>,
}

impl std::fmt::Debug for FrozenTimeModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrozenTimeModel")
            .field("inner", &self.inner)
            .field("expired", &self.expired)
            .field("triggers", &format!("[{} triggers]", self.triggers.len()))
            .finish()
    }
}

impl FrozenTimeModel {
    /// Create a new Frozen time model
    pub fn new() -> Self {
        Self::default()
    }

    /// Expire all timeouts on tasks that satisfy a predicate
    pub fn trigger_timeouts<F>(&mut self, trigger: F)
    where
        F: Fn(&Labels) -> bool + 'static,
    {
        let num_tasks = ExecutionState::with(|s| s.num_tasks());
        for i in 0..num_tasks {
            let task_id = TaskId::from(i);
            with_labels_for_task(task_id, |labels| {
                if trigger(labels) {
                    self.expired.insert(task_id);
                }
            });
        }

        let mut to_wake = Vec::new();
        for Reverse((_, task_id, sleep_id)) in self.inner.get_waiters() {
            if self.expired.contains(task_id) {
                to_wake.push(*sleep_id);
            }
        }

        for sleep_id in to_wake {
            self.inner.wake_frozen(sleep_id);
        }

        self.triggers.push(Box::new(trigger));
    }

    /// Clear all triggers that expire timeouts
    pub fn clear_triggers(&mut self) {
        self.expired.clear();
        self.triggers.clear();
    }
}

impl Default for FrozenTimeModel {
    fn default() -> Self {
        Self {
            inner: ConstantSteppedTimeModel::new(std::time::Duration::ZERO),
            expired: HashSet::new(),
            triggers: Vec::new(),
        }
    }
}

impl Clone for FrozenTimeModel {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            expired: self.expired.clone(),
            triggers: Vec::new(), // Don't clone triggers
        }
    }
}

impl TimeModel for FrozenTimeModel {
    fn pause(&mut self) {
        warn!("Pausing frozen model has no effect")
    }

    fn resume(&mut self) {
        warn!("Resuming frozen model has no effect")
    }

    fn step(&mut self) {}

    fn new_execution(&mut self) {
        self.inner.new_execution();
        self.expired.clear();
        self.triggers.clear();
    }

    fn instant(&self) -> Instant {
        self.inner.instant()
    }

    fn wake_next(&mut self) -> bool {
        self.inner.wake_next()
    }

    fn advance(&mut self, dur: Duration) {
        self.inner.advance(dur);
    }

    fn register_sleep(&mut self, deadline: Instant, sleep_id: u64, waker: Option<Waker>) -> bool {
        let task_id = ExecutionState::me();
        for trigger in &self.triggers {
            with_labels_for_task(task_id, |labels| {
                if trigger(labels) {
                    self.expired.insert(task_id);
                }
            });
        }

        if !self.expired.contains(&task_id) {
            self.inner.register_sleep(deadline, sleep_id, waker)
        } else {
            true
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
