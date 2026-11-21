//! Per-execution state for the weak-memory emulation of atomics.
//!
//! The emulation itself, with the store buffer of each atomic, lives with the atomics in
//! `shuttle-std` (`sync::atomic::weak_memory`). It is a port of Miri's, and this module holds what
//! Miri keeps in its data race detector: a few clocks for each task besides the task's own vector
//! clock, and two clocks shared by the whole execution for SeqCst operations.

use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::TaskId;

/// The clocks the weak-memory emulation keeps for one task, besides the task's own vector clock.
/// They are Miri's `ThreadClockSet`. A new task starts with all of them empty.
#[derive(Clone, Debug)]
pub struct TaskWeakMemoryClocks {
    /// What the task's next Acquire fence acquires: the release clocks of the stores that the
    /// task's Relaxed loads have read.
    pub fence_acquire: VectorClock,

    /// The task's clock at its last Release fence. This is what the task's Relaxed stores release.
    pub fence_release: VectorClock,

    /// For each task, the time of its last SeqCst store that this task's SeqCst fences have seen.
    /// A load can't read a SeqCst store older than that. It is never joined into the task's clock.
    pub write_seqcst: VectorClock,

    /// For each task, the time of its last SeqCst fence that this task's SeqCst loads have seen. A
    /// SeqCst load can't read a store older than the last one that task made before that fence. It
    /// is never joined into the task's clock.
    pub read_seqcst: VectorClock,
}

impl TaskWeakMemoryClocks {
    const fn new() -> Self {
        Self {
            fence_acquire: VectorClock::new(),
            fence_release: VectorClock::new(),
            write_seqcst: VectorClock::new(),
            read_seqcst: VectorClock::new(),
        }
    }
}

/// The weak-memory clocks of every task in an execution.
#[derive(Debug, Default)]
pub struct WeakMemoryTasks(Vec<TaskWeakMemoryClocks>);

impl WeakMemoryTasks {
    /// The weak-memory clocks of the given task.
    pub fn get(&mut self, task: TaskId) -> &mut TaskWeakMemoryClocks {
        let i = usize::from(task);
        if self.0.len() <= i {
            self.0.resize_with(i + 1, TaskWeakMemoryClocks::new);
        }
        &mut self.0[i]
    }
}

/// The state of the weak-memory emulation for one execution.
#[derive(Debug)]
pub struct WeakMemoryState {
    /// A number that no other execution in this process has. A static atomic outlives the
    /// execution that stored to it, so its store buffer records this to tell when it has to start
    /// over.
    pub execution: u64,

    /// The clocks of each task.
    pub tasks: WeakMemoryTasks,

    /// The join of the clocks of all SeqCst fences so far. Each SeqCst fence joins its task's
    /// clock into it and acquires it, as if it were an AcqRel read-modify-write of one global
    /// location. Miri does the same. It rules out some outcomes that C++20 allows.
    pub last_sc_fence: VectorClock,

    /// For each task, the time of its last SeqCst store.
    pub last_sc_write: VectorClock,
}

impl WeakMemoryState {
    pub(crate) fn new() -> Self {
        // Starts at 1, so that a store buffer that has never been used doesn't match any execution.
        static NEXT_EXECUTION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            execution: NEXT_EXECUTION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            tasks: WeakMemoryTasks::default(),
            last_sc_fence: VectorClock::new(),
            last_sc_write: VectorClock::new(),
        }
    }
}
