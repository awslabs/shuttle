//! State that a structure keeps for one Shuttle execution.
//!
//! Each of the crossbeam wrappers' inner crates has a copy of this module; keep the copies the same.
//! The crate that includes it names the second half of its storage keys in `crate::STORAGE_TAG`.
//!
//! A structure holds no state of its own. It holds an identifier, assigned on its first use, and its
//! state lives in the storage of the running execution under that identifier. So a structure that
//! outlives an execution, a `static` or one that the test closure captured, starts every execution as
//! it was constructed, and two tests that share a `static` on different threads do not see each
//! other's state. The constructors stay `const` where crossbeam's are.
//!
//! The state is dropped when the structure is dropped. A structure that is still alive when the
//! execution ends has its state dropped with the execution's other storage, as Shuttle drops the
//! values of `lazy_static!`s. The storage holds the state type-erased, so that the state may borrow,
//! as crossbeam's structures may: the structure cannot outlive what its state borrows, and the state
//! is dropped no later than the structure, unless the structure is leaked with `mem::forget` or a
//! reference cycle, which a borrowing destructor in the state must not be combined with.

use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::storage::StorageKey;
use shuttle_engine::runtime::task::clock::VectorClock;
use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Source of the identifiers, shared by every structure of this crate, so that an identifier belongs
/// to one structure only.
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

/// A slot of the execution storage: a `Box<RefCell<S>>` with its type erased, or null once the
/// structure took its state back.
struct Slot {
    state: Cell<*mut ()>,
    drop_state: unsafe fn(*mut ()),
}

impl Drop for Slot {
    fn drop(&mut self) {
        let state = self.state.replace(ptr::null_mut());
        if !state.is_null() {
            // Safety: `state` came from `Box::into_raw` of the type that `drop_state` drops.
            unsafe { (self.drop_state)(state) }
        }
    }
}

/// Drops a state that [`PerExecution::get`] boxed.
///
/// # Safety
///
/// `state` must come from `Box::into_raw` of a `Box<RefCell<S>>`.
unsafe fn drop_state<S>(state: *mut ()) {
    drop(unsafe { Box::from_raw(state.cast::<RefCell<S>>()) });
}

/// The state of type `S` that a structure keeps for each execution (see the module documentation).
pub(crate) struct PerExecution<S> {
    /// The first half of the structure's storage key. Zero until the structure's first use, so that
    /// the constructor can be `const`.
    id: AtomicUsize,
    /// The structure owns an `S`, by way of the execution's storage.
    _state: PhantomData<S>,
}

impl<S> PerExecution<S> {
    /// A structure that has not been used yet.
    pub(crate) const fn new() -> Self {
        Self {
            id: AtomicUsize::new(0),
            _state: PhantomData,
        }
    }

    fn key(&self) -> StorageKey {
        let mut id = self.id.load(Ordering::Relaxed);
        if id == 0 {
            let fresh = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            assert_ne!(fresh, 0, "identifier overflow");
            id = match self.id.compare_exchange(0, fresh, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => fresh,
                Err(assigned) => assigned,
            };
        }
        StorageKey(id, crate::STORAGE_TAG)
    }

    /// This execution's state, which `init` creates on the structure's first use in the execution.
    ///
    /// Panics outside of a Shuttle execution. The reference must not be kept across a scheduling
    /// point, nor across a call into code that may use the structure.
    pub(crate) fn get(&self, init: impl FnOnce() -> S) -> &RefCell<S> {
        let key = self.key();
        let existing = ExecutionState::with(|state| state.get_storage::<_, Slot>(key).map(|slot| slot.state.get()));
        let state = match existing {
            Some(state) if !state.is_null() => state,
            // There is no state while the structure is being dropped or consumed, which no `&self`
            // outlives; create a fresh one rather than fail.
            Some(_) => {
                let state = Box::into_raw(Box::new(RefCell::new(init()))).cast::<()>();
                ExecutionState::with(|execution| {
                    let slot = execution
                        .get_storage::<_, Slot>(key)
                        .expect("the slot was there just now");
                    slot.state.set(state);
                });
                state
            }
            None => {
                // `init` runs before the execution's state is borrowed, so that it may use Shuttle.
                let state = Box::into_raw(Box::new(RefCell::new(init()))).cast::<()>();
                ExecutionState::with(|execution| {
                    execution.init_storage(
                        key,
                        Slot {
                            state: Cell::new(state),
                            drop_state: drop_state::<S>,
                        },
                    )
                });
                state
            }
        };
        // Safety: the slot keeps the box at this address until the structure takes it back, which
        // needs `&mut self` or `self`, or until the execution ends, which no task's use of a
        // structure spans (see above).
        unsafe { &*state.cast::<RefCell<S>>() }
    }

    /// Takes this execution's state back from the storage, if the structure has used it in this
    /// execution.
    ///
    /// Returns `None` outside of an execution, and while the execution is being torn down, when the
    /// structure's slot may have been dropped already: then the storage drops the state itself, if
    /// it has not yet.
    pub(crate) fn take(&mut self) -> Option<S> {
        let id = *self.id.get_mut();
        if id == 0 {
            return None;
        }
        let key = StorageKey(id, crate::STORAGE_TAG);
        let state = ExecutionState::try_with(|execution| {
            if execution.in_cleanup() {
                return None;
            }
            execution
                .get_storage::<_, Slot>(key)
                .map(|slot| slot.state.replace(ptr::null_mut()))
        })
        .ok()
        .flatten()?;
        if state.is_null() {
            return None;
        }
        // Safety: the slot held this pointer, which `get` created with this type, and gave it up.
        Some(unsafe { Box::from_raw(state.cast::<RefCell<S>>()) }.into_inner())
    }
}

impl<S> Drop for PerExecution<S> {
    fn drop(&mut self) {
        // Dropped here, after the execution's state is no longer borrowed, since the state's
        // destructors may use Shuttle.
        drop(self.take());
    }
}

/// Whether the current thread runs a Shuttle execution.
#[allow(dead_code)]
pub(crate) fn in_execution() -> bool {
    ExecutionState::try_with(|_| ()).is_ok()
}

/// Makes the current task and a structure synchronize, as an operation on one of Shuttle's atomics
/// does: the task's clock takes in the structure's, and the structure's takes in the task's.
#[allow(dead_code)]
pub(crate) fn synchronize(clock: &mut VectorClock) {
    ExecutionState::with(|execution| {
        execution.update_clock(clock);
        clock.update(execution.increment_clock());
    });
}
