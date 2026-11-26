//! What every primitive in this crate is built on: a lock word that lives in the running
//! execution, and the tasks that wait for it to change.
//!
//! spin's primitives are an atomic word and a loop that retries an operation on it until the
//! operation succeeds. Each primitive here keeps the same word, with the same encoding, in the
//! storage of the running execution, and runs the same operations on it. An operation is one step:
//! a scheduling point, then the update of the word, as one of Shuttle's atomics is. Where spin
//! would spin, the task instead blocks until another task changes the word, and then retries.
//! That reaches the same states: until another task changes the word, a retry fails again.
//!
//! The word starts afresh in every execution, so a `static` lock is unlocked at the start of each
//! one, with no waiters, and a `static` [`Once`](crate::once::Once) is empty again.

use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::per_execution::ValueLock;
use shuttle_engine::runtime::storage::StorageKey;
use shuttle_engine::runtime::task::TaskId;
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::thread;
use std::cell::{Cell, RefCell};
use std::sync::LazyLock as StdLazyLock;
use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};

/// Source of the identifiers that key the primitives' per-execution state, so that an identifier
/// belongs to one primitive only.
static NEXT_ID: StdAtomicUsize = StdAtomicUsize::new(1);

/// The second half of every key this crate uses. It tells these slots apart from Shuttle's own,
/// whose identifiers come from other counters (`Once` uses 0x2, `OnceLock` 0x4) or are addresses
/// (`lazy_static` uses 0x3).
const TAG: usize = 0x7370_696e;

fn next_id() -> usize {
    let id = NEXT_ID.fetch_add(1, StdOrdering::Relaxed);
    assert_ne!(id, 0, "id overflow");
    id
}

/// The part of a primitive that finds its state in the running execution.
pub(crate) struct Core {
    /// Keys the primitive's state. Assigned on first use, so that constructors can be `const`, as
    /// Shuttle's `Once` does.
    id: StdLazyLock<usize>,
}

impl Core {
    pub(crate) const fn new() -> Self {
        Self {
            id: StdLazyLock::new(next_id),
        }
    }

    fn key(&self) -> StorageKey {
        StorageKey(*self.id, TAG)
    }

    /// The primitive's state in the running execution, created with `init` if the primitive has not
    /// been used in this execution yet. The `bool` says whether this call created it.
    ///
    /// Panics outside of a Shuttle execution, as Shuttle's primitives do.
    ///
    /// The state lives in the execution's storage, which keeps it at the same address until the
    /// execution ends, so the reference stays valid for as long as the operation that asked for it
    /// runs, including across scheduling points. Callers must not keep it beyond that: a `static`
    /// outlives the execution.
    pub(crate) fn state<X: 'static>(&self, init: impl FnOnce() -> State<X>) -> (&State<X>, bool) {
        let key = self.key();
        let (state, created) = ExecutionState::with(|execution| {
            let created = execution.get_storage::<_, State<X>>(key).is_none();
            if created {
                execution.init_storage(key, init());
            }
            let state = execution
                .get_storage::<_, State<X>>(key)
                .expect("the state was just initialized") as *const State<X>;
            (state, created)
        });
        // Safety: see above.
        (unsafe { &*state }, created)
    }

    /// Like [`Self::state`], but returns `None` where there is no execution to hold the state, or
    /// where its state cannot be borrowed: outside of an execution (a primitive that outlives its
    /// test), and while Shuttle holds the state itself (execution teardown dropping a value).
    pub(crate) fn try_state<X: 'static>(&self, init: impl FnOnce() -> State<X>) -> Option<(&State<X>, bool)> {
        let key = self.key();
        let (state, created) = ExecutionState::try_with(|execution| {
            let created = execution.get_storage::<_, State<X>>(key).is_none();
            if created {
                execution.init_storage(key, init());
            }
            let state = execution
                .get_storage::<_, State<X>>(key)
                .expect("the state was just initialized") as *const State<X>;
            (state, created)
        })
        .ok()?;
        // Safety: see `state`.
        Some((unsafe { &*state }, created))
    }

    /// The primitive's state in the running execution, if it has been used in this execution.
    pub(crate) fn existing_state<X: 'static>(&self) -> Option<&State<X>> {
        let key = self.key();
        let state = ExecutionState::try_with(|execution| {
            execution
                .get_storage::<_, State<X>>(key)
                .map(|state| state as *const State<X>)
        })
        .ok()??;
        // Safety: see `state`.
        Some(unsafe { &*state })
    }
}

/// A primitive's state in one execution.
pub(crate) struct State<X> {
    /// The word spin keeps in an atomic, with spin's encoding.
    pub(crate) word: Cell<usize>,
    /// What else a primitive needs (a ticket lock's turn, a `Once`'s value).
    pub(crate) extra: X,
    /// The join of the clocks of the tasks that changed the word, so that a task that reads the word
    /// is causally after every change it could have seen. Shuttle treats every atomic operation as
    /// `SeqCst`, and this does the same.
    clock: RefCell<VectorClock>,
    /// The tasks blocked until the word changes.
    waiters: RefCell<Vec<TaskId>>,
}

impl<X> State<X> {
    pub(crate) fn new(word: usize, extra: X) -> Self {
        Self {
            word: Cell::new(word),
            extra,
            clock: RefCell::new(VectorClock::new()),
            waiters: RefCell::new(Vec::new()),
        }
    }

    /// Makes the current task causally after every change to the word so far: the reading half of
    /// an operation.
    pub(crate) fn observe(&self) {
        let clock = self.clock.borrow().clone();
        ExecutionState::with(|execution| execution.update_clock(&clock));
    }

    /// Records a change by the current task, for the tasks that read the word later: the writing
    /// half of an operation.
    pub(crate) fn publish(&self) {
        let clock = ExecutionState::with(|execution| execution.increment_clock().clone());
        self.clock.borrow_mut().update(&clock);
    }

    /// Blocks the current task until another task calls [`Self::wake`].
    ///
    /// Call it right after an operation on the word failed, with no scheduling point in between,
    /// so that no change can come between the failure and the wait and be missed.
    pub(crate) fn wait(&self) {
        let me = ExecutionState::me();
        self.waiters.borrow_mut().push(me);

        // Execution teardown can unwind the task from the switch below. It is then no longer
        // waiting, and its destructors, which run as the task, may change the word.
        struct StopWaiting<'a, X>(&'a State<X>, TaskId);
        impl<X> Drop for StopWaiting<'_, X> {
            fn drop(&mut self) {
                self.0.waiters.borrow_mut().retain(|task| *task != self.1);
            }
        }
        let stop_waiting = StopWaiting(self, me);

        ExecutionState::with(|execution| execution.current_mut().block(false));
        thread::switch();
        drop(stop_waiting);
    }

    /// Changes the word with `change`, as a release (an unlock, a downgrade, the end of an
    /// initialization), and wakes the tasks that wait for the word to change. After the execution
    /// has stopped, there is nobody to wake and no clock to publish, but the word still changes, so
    /// that destructors that run later find the lock released.
    pub(crate) fn release(&self, change: impl FnOnce(&Self)) {
        change(self);
        if !ExecutionState::should_stop() {
            self.publish();
            self.wake();
        }
    }

    /// Unblocks every task waiting for the word to change. Not a scheduling point: the operation
    /// that changed the word has taken one already. A woken task retries its operation, and waits
    /// again if it still fails.
    pub(crate) fn wake(&self) {
        let waiters = std::mem::take(&mut *self.waiters.borrow_mut());
        // An execution that has stopped has no tasks left to run (see `ExecutionState::should_stop`).
        if waiters.is_empty() || ExecutionState::should_stop() {
            return;
        }
        ExecutionState::with(|execution| {
            for task in waiters {
                let task = execution.get_mut(task);
                if !task.finished() {
                    task.unblock();
                }
            }
        });
    }
}

/// Runs `release` after a scheduling point, as the release of a lock: the step that lets the task
/// that waits for it run.
///
/// Execution teardown can unwind a task from that scheduling point, which is often in a guard's
/// destructor. `release` runs then too, without the scheduling point, so that destructors that run
/// later find the lock released, as Shuttle's own locks do. Outside of an execution, and after the
/// execution has stopped, there is no scheduling point to take.
pub(crate) fn release_after_switch(release: impl FnOnce()) {
    struct ReleaseOnUnwind<F: FnOnce()>(Option<F>);
    impl<F: FnOnce()> Drop for ReleaseOnUnwind<F> {
        fn drop(&mut self) {
            if let Some(release) = self.0.take() {
                release();
            }
        }
    }
    let mut release = ReleaseOnUnwind(Some(release));
    thread::switch_if_not_stopping();
    (release.0.take().expect("released once"))();
}

/// Takes a lock of spin's own, which is where a lock of this crate keeps its data, for a new guard,
/// after the task has acquired the lock in this execution.
///
/// The lock's `ValueLock` comes first, which keeps the data, and what the lock guards outside of
/// itself, to one execution at a time. It panics for a guard of another execution: one of a test
/// that runs at the same time on another thread and uses the same `static`, or one that an ended
/// execution left behind, forgotten with `mem::forget`, say (see `shuttle-engine`'s `ValueLock`).
/// `kind` names the lock in the message. Past it, spin's lock is free, unless a `Debug` on another
/// thread holds it for a moment, which [`retake_data`] waits out.
pub(crate) fn lock_data<G>(value: &ValueLock, kind: &'static str, try_lock: impl FnMut() -> Option<G>) -> G {
    value.acquire(kind);
    retake_data(try_lock)
}

/// Takes spin's lock around a lock's data for an access that holds the lock's `ValueLock` already:
/// a new guard (see [`lock_data`]), an upgrade, or the downgrade of a forgotten guard, which let
/// spin's guard go and take another. No other execution can hold spin's lock then, and no other
/// task of this one, so only a `Debug` on another thread can, while it formats the value.
pub(crate) fn retake_data<G>(mut try_lock: impl FnMut() -> Option<G>) -> G {
    loop {
        if let Some(guard) = try_lock() {
            return guard;
        }
        std::thread::yield_now();
    }
}
