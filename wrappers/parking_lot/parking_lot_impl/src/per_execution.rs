//! Per-execution state for the primitives of this crate.
//!
//! Every primitive here can be `const`-constructed, so it can live in a `static` and outlive a
//! Shuttle execution, and two Shuttle tests running at the same time on different threads can share
//! it. So a primitive doesn't hold its Shuttle state (a lock's semaphore, a condvar's waiters, the
//! progress of a `Once`) itself. It holds a [`Handle`]: an id, assigned when it is first used. Its
//! state lives with the running execution, created at its first use in each execution. Every
//! execution therefore starts with every primitive in its initial state, as `shuttle::lazy_static!`
//! and `shuttle::sync::Once` do, and an execution never sees the state of one running on another
//! thread.
//!
//! The states of one execution sit in one slot of the execution's storage, which Shuttle drops when
//! it tears the execution down. Statics go last in a teardown, one at a time, and the destructor of
//! a later one can still lock a `static` mutex whose state was in the slot. Shuttle panics if storage
//! is read after its slot is gone, so this module also keeps the states in a thread-local, which a
//! teardown uses once the slot is gone (see [`states`]). Executions run on the thread of the test
//! that started them, one after another, so a thread-local can stand for the execution.
//!
//! The protected data of a lock is not part of its state: `lock_api` keeps it inside the lock, so a
//! `static` lock's data carries over from one execution to the next.

use shuttle_engine::current::TaskId;
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::storage::StorageKey;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The next id for a [`Handle`]. Ids are unique in the process; 0 means "not assigned yet".
static NEXT_HANDLE_ID: AtomicUsize = AtomicUsize::new(1);

/// The next id for [`thread_id`] to hand out. Ids are unique in the process.
static NEXT_THREAD_ID: AtomicUsize = AtomicUsize::new(1);

/// Its address keys this copy of the crate's slot, so that two versions of the crate linked into
/// one test don't share a slot.
static SLOT_ANCHOR: u8 = 0;

fn slot_key() -> StorageKey {
    // The second half is the slot's type in `StorageKey`'s terms; "plot", for parking_lot.
    StorageKey(&SLOT_ANCHOR as *const u8 as usize, 0x706c_6f74)
}

/// The states of the primitives that one execution has used, and the thread ids it has handed out.
#[derive(Default)]
struct States {
    by_handle: RefCell<HashMap<usize, Rc<dyn Any>>>,
    thread_ids: RefCell<HashMap<TaskId, NonZeroUsize>>,
}

/// What this crate keeps in an execution's storage.
struct Slot(Rc<States>);

impl Drop for Slot {
    fn drop(&mut self) {
        // Shuttle drops the slot while it tears the execution down. From here to the end of the
        // teardown, `states` must not look the slot up again.
        let _ = SLOT_DROPPED.try_with(|dropped| dropped.set(true));
    }
}

thread_local! {
    /// The states of the execution that last used this crate on this thread. They stay until the
    /// next execution on this thread uses the crate, so that the end of a teardown can still use
    /// them after the slot that holds them is gone.
    static CURRENT: RefCell<Option<Rc<States>>> = const { RefCell::new(None) };

    /// Whether the slot that holds `CURRENT` has been dropped.
    static SLOT_DROPPED: Cell<bool> = const { Cell::new(false) };
}

/// The states of the running execution, created at its first use of this crate.
///
/// Panics outside of a Shuttle execution, as every Shuttle primitive does.
fn states() -> Rc<States> {
    // In a teardown, after the slot was dropped: looking it up again would panic, and `CURRENT` is
    // what it held. The one case this gets wrong is an execution whose first use of the crate comes
    // after this point in its own teardown, from a static's destructor: it gets the states the
    // previous execution on this thread left behind.
    if SLOT_DROPPED.with(Cell::get)
        && ExecutionState::with(|state| state.in_cleanup())
        && let Some(states) = CURRENT.with(|current| current.borrow().clone())
    {
        return states;
    }

    let slot = ExecutionState::with(|state| state.get_storage::<_, Slot>(slot_key()).map(|slot| Rc::clone(&slot.0)));
    if let Some(states) = slot {
        return states;
    }

    // The first use in this execution.
    let states = Rc::new(States::default());
    ExecutionState::with(|state| state.init_storage(slot_key(), Slot(Rc::clone(&states))));
    let previous = CURRENT.with(|current| current.replace(Some(Rc::clone(&states))));
    SLOT_DROPPED.with(|dropped| dropped.set(false));
    // The previous execution's states go here, outside of the borrows above.
    drop(previous);
    states
}

/// A primitive's link to its state in the running execution.
#[derive(Debug)]
pub(crate) struct Handle {
    id: AtomicUsize,
}

impl Handle {
    pub(crate) const fn new() -> Self {
        Self {
            id: AtomicUsize::new(0),
        }
    }

    /// This handle's id, assigned at the first call.
    pub(crate) fn id(&self) -> usize {
        let id = self.id.load(Ordering::Relaxed);
        if id != 0 {
            return id;
        }
        let new = NEXT_HANDLE_ID.fetch_add(1, Ordering::Relaxed);
        assert_ne!(new, 0, "parking_lot handle id overflow");
        match self.id.compare_exchange(0, new, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => new,
            Err(assigned) => assigned,
        }
    }

    /// The primitive's state in the running execution. `init` creates it at the primitive's first
    /// use in the execution.
    ///
    /// The state is shared through an `Rc`, so a caller can hold it across a scheduling point, while
    /// other tasks use the primitive.
    pub(crate) fn state<T: 'static>(&self, init: impl FnOnce() -> T) -> Rc<T> {
        let id = self.id();
        let states = states();
        let existing = states.by_handle.borrow().get(&id).cloned();
        let state = match existing {
            Some(state) => state,
            None => {
                let state: Rc<dyn Any> = Rc::new(init());
                states.by_handle.borrow_mut().insert(id, Rc::clone(&state));
                state
            }
        };
        state
            .downcast::<T>()
            .unwrap_or_else(|_| unreachable!("the state of a handle has one type"))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let id = *self.id.get_mut();
        if id == 0 {
            return;
        }
        // Free the state now rather than at the end of the execution, which matters for a test
        // that creates many short-lived locks. Nobody can use it again: the id goes with the
        // primitive. This runs wherever the primitive is dropped (outside of an execution, during
        // a teardown, from a thread-local's destructor), so it touches nothing but the
        // thread-local, and gives up rather than panics if that is unavailable.
        let removed = CURRENT
            .try_with(|current| {
                let current = current.try_borrow().ok()?;
                let mut by_handle = current.as_ref()?.by_handle.try_borrow_mut().ok()?;
                by_handle.remove(&id)
            })
            .ok()
            .flatten();
        drop(removed);
    }
}

/// An id for the running task, unique among the tasks of every execution in the process.
///
/// Shuttle numbers the tasks of each execution from zero, so a task id alone would let a
/// `ReentrantMutex` that a task of one execution left locked take a task of a later execution for
/// its owner.
pub(crate) fn thread_id() -> NonZeroUsize {
    let me = ExecutionState::me();
    let states = states();
    let mut ids = states.thread_ids.borrow_mut();
    *ids.entry(me).or_insert_with(|| {
        NonZeroUsize::new(NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed)).expect("parking_lot thread id overflow")
    })
}
