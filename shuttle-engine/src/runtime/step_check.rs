//! The scheduling-point checker, built with the `check-scheduling-points` feature.
//!
//! A *step* is the code that a task runs between two consecutive scheduling points. Shuttle can only
//! interleave tasks at scheduling points, so each step should perform at most one operation that
//! other tasks can observe: if a step accesses two different shared resources, and at least one of
//! the accesses is a write, no execution runs another task between them. The checker fails a test
//! as soon as a step does that. It sees only the accesses that primitives report to it with
//! [`ExecutionState::record_access`](crate::runtime::execution::ExecutionState::record_access).
//!
//! Resources are identified by the address of their [`ResourceSignature`], not by the signature
//! itself: resources with `const` constructors (atomics, say) have the same signature if they are
//! created at the same source location, and two of them accessed in one step would look like one.
//! Within a step, a resource doesn't move, as other tasks can share it.

use crate::runtime::task::TaskId;
use crate::sync_types::{AccessKind, ResourceSignature};
use smallvec::SmallVec;
use std::fmt::Write;
use std::panic::Location;

/// The accesses that the current step has made so far.
#[derive(Debug, Default)]
pub(crate) struct StepAccesses {
    // The step that `accesses` belong to, as the number of scheduling decisions made before it began
    // (see `ExecutionState::context_switches`). Every scheduling decision ends a step, so a log of an
    // earlier step is stale, and cleared by the first access of the next. This keeps the checker's
    // bookkeeping out of scheduling points, except for `step_starts`.
    step: usize,
    accesses: SmallVec<[Access; 4]>,
    // The scheduling point at which each task's current step began, indexed by task ID, if the step
    // began at a scheduling point rather than at the start of the task.
    step_starts: Vec<Option<&'static Location<'static>>>,
    // How deep in `ExecutionState::atomic_step` scopes the current task is, and the group of the
    // outermost scope, if it is in one.
    atomic_depth: usize,
    atomic_group: Option<usize>,
    next_atomic_group: usize,
}

#[derive(Debug)]
struct Access {
    resource: usize,
    signature: ResourceSignature,
    kind: AccessKind,
    location: &'static Location<'static>,
    // The `atomic_step` scope the access was made in, if any. Accesses in the same scope are one
    // operation, and don't conflict with each other.
    atomic_group: Option<usize>,
}

impl StepAccesses {
    /// The task `task` reached a scheduling point at `location`.
    pub(crate) fn scheduling_point(&mut self, task: TaskId, location: &'static Location<'static>) {
        assert_eq!(
            self.atomic_depth, 0,
            "a task reached a scheduling point at {location} inside `ExecutionState::atomic_step`, which must \
            be a single step"
        );
        let task = usize::from(task);
        if self.step_starts.len() <= task {
            self.step_starts.resize(task + 1, None);
        }
        self.step_starts[task] = Some(location);
    }

    /// Record an access that task `task` makes in step `step`. Returns a description of the
    /// violation if the step now accesses two distinct resources, and at least one access is a write.
    pub(crate) fn record(
        &mut self,
        step: usize,
        task: TaskId,
        signature: &ResourceSignature,
        kind: AccessKind,
        location: &'static Location<'static>,
    ) -> Option<String> {
        if self.step != step {
            self.step = step;
            self.accesses.clear();
        }
        let access = Access {
            resource: signature as *const ResourceSignature as usize,
            signature: signature.clone(),
            kind,
            location,
            atomic_group: self.atomic_group,
        };
        let conflicts = self.accesses.iter().any(|other| access.conflicts_with(other));
        self.accesses.push(access);
        conflicts.then(|| self.describe_violation(task))
    }

    pub(crate) fn enter_atomic_step(&mut self) {
        if self.atomic_depth == 0 {
            self.atomic_group = Some(self.next_atomic_group);
            self.next_atomic_group += 1;
        }
        self.atomic_depth += 1;
    }

    pub(crate) fn exit_atomic_step(&mut self) {
        self.atomic_depth -= 1;
        if self.atomic_depth == 0 {
            self.atomic_group = None;
        }
    }

    #[cold]
    fn describe_violation(&self, task: TaskId) -> String {
        let start = match self.step_starts.get(usize::from(task)).copied().flatten() {
            Some(location) => format!("the scheduling point at {location}"),
            None => "the start of the task".to_string(),
        };
        let mut message = format!(
            "missing scheduling point: a step of the current task accessed two shared resources, at least one of \
            them for writing, with no scheduling point in between, so no execution runs another task between \
            the accesses. Put a scheduling point before the second access, or, if the accesses form one atomic \
            operation, wrap them in `ExecutionState::atomic_step`. The step began at {start}, and made these \
            accesses:"
        );
        for access in &self.accesses {
            let _ = write!(
                message,
                "\n  {:?} of {} at {}",
                access.kind, access.signature, access.location
            );
        }
        message
    }
}

impl Access {
    fn conflicts_with(&self, other: &Access) -> bool {
        self.resource != other.resource
            && (self.kind == AccessKind::Write || other.kind == AccessKind::Write)
            && (self.atomic_group.is_none() || self.atomic_group != other.atomic_group)
    }
}
