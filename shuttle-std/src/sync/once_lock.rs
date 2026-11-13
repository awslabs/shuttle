use crate::sync::{Mutex, ResourceSignature, ResourceType};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::storage::StorageKey;
use shuttle_engine::runtime::task::clock::VectorClock;
use std::cell::{Cell, RefCell};
use std::convert::Infallible;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering};
use std::sync::LazyLock as StdLazyLock;
use tracing::trace;

/// Source of the identifiers that key cells' per-execution storage. Shared by every [`OnceCore`],
/// whatever its type parameter, so that an identifier belongs to one cell only.
static NEXT_ID: StdAtomicUsize = StdAtomicUsize::new(1);

/// A cell which can be written to only once, the same as [`std::sync::OnceLock`].
///
/// # Differences from `std`
///
/// The value lives in the current execution's storage rather than in the cell, so a `static`
/// `OnceLock` is empty again at the start of every execution, as [`Once`](crate::sync::Once) and
/// [`shuttle::lazy_static!`] are. A value that carried over would make an execution depend on the
/// ones before it, and so make a failing schedule impossible to replay. The rest of the
/// differences follow from where the value lives:
///
/// * The methods that reach the value require `T: 'static`, which the execution's storage requires.
/// * They have to run inside a Shuttle execution, including those of [`Debug`], which reads the
///   value.
/// * A value is dropped when the execution ends, not when the cell is dropped, so a `OnceLock`
///   that goes out of scope part way through an execution keeps its value until the end of it.
///
/// [`OnceLock::wait`](std::sync::OnceLock::wait), which blocks until the cell has a value, and the
/// `Clone`, `From<T>`, `PartialEq` and `Eq` impls are not modelled yet.
///
/// [`shuttle::lazy_static!`]: https://docs.rs/shuttle/*/shuttle/macro.lazy_static.html
pub struct OnceLock<T> {
    core: OnceCore<T>,
}

impl<T> OnceLock<T> {
    /// Creates a new empty cell.
    #[must_use]
    #[track_caller]
    pub const fn new() -> Self {
        Self {
            core: OnceCore::new(ResourceType::OnceLock),
        }
    }
}

impl<T: 'static> OnceLock<T> {
    /// Gets the reference to the underlying value.
    ///
    /// Returns `None` if the cell is uninitialized, or being initialized. This method never blocks
    /// and is not a scheduling point.
    pub fn get(&self) -> Option<&T> {
        self.core.get()
    }

    /// Gets the mutable reference to the underlying value.
    ///
    /// Returns `None` if the cell is uninitialized. This method never blocks and is not a
    /// scheduling point.
    pub fn get_mut(&mut self) -> Option<&mut T> {
        self.core.get_mut()
    }

    /// Sets the contents of this cell to `value`.
    ///
    /// May block if another task is currently attempting to initialize the cell. The cell is
    /// guaranteed to contain a value when `set` returns, though not necessarily the one provided.
    ///
    /// Returns `Ok(())` if the cell was uninitialized and `Err(value)` if it was already
    /// initialized.
    pub fn set(&self, value: T) -> Result<(), T> {
        match self.try_insert(value) {
            Ok(_) => Ok(()),
            Err((_, value)) => Err(value),
        }
    }

    /// Initializes the contents of the cell to `value` if the cell was uninitialized, then returns
    /// a reference to it.
    ///
    /// Returns `Ok(&value)` if the cell was uninitialized and `Err((&current_value, value))` if it
    /// was already initialized.
    pub fn try_insert(&self, value: T) -> Result<&T, (&T, T)> {
        let mut value = Some(value);
        let result = self.get_or_init(|| value.take().expect("the initializer runs at most once"));
        match value {
            None => Ok(result),
            Some(value) => Err((result, value)),
        }
    }

    /// Gets the contents of the cell, initializing it with `f` if the cell was uninitialized.
    ///
    /// Many tasks may call `get_or_init` concurrently with different initializing functions, but
    /// it is guaranteed that only one function will be executed.
    ///
    /// # Panics
    ///
    /// If `f` panics, the panic is propagated to the caller, and the cell remains uninitialized: a
    /// later call runs its own initializer.
    ///
    /// It is an error to reentrantly initialize the cell from `f`. Shuttle panics with a deadlock
    /// error, where std's current implementation deadlocks.
    pub fn get_or_init<F>(&self, f: F) -> &T
    where
        F: FnOnce() -> T,
    {
        match self.get_or_try_init(|| Ok::<T, Infallible>(f())) {
            Ok(value) => value,
            Err(infallible) => match infallible {},
        }
    }

    /// Gets the contents of the cell, initializing it with `f` if the cell was uninitialized. If
    /// the cell was uninitialized and `f` failed, an error is returned.
    ///
    /// # Panics
    ///
    /// If `f` panics, the panic is propagated to the caller, and the cell remains uninitialized.
    ///
    /// It is an error to reentrantly initialize the cell from `f`. Shuttle panics with a deadlock
    /// error, where std's current implementation deadlocks.
    pub fn get_or_try_init<F, E>(&self, f: F) -> Result<&T, E>
    where
        F: FnOnce() -> Result<T, E>,
    {
        // A `OnceLock` whose initializer panicked or failed can be initialized again, as std's
        // can: std ignores the poisoning of the `Once` it is built on.
        self.core.get_or_try_init(IfPoisoned::Retry, f)
    }

    /// Takes the value out of this `OnceLock`, moving it back to an uninitialized state.
    ///
    /// Has no effect and returns `None` if the `OnceLock` was uninitialized.
    pub fn take(&mut self) -> Option<T> {
        self.core.take()
    }

    /// Consumes the `OnceLock`, returning the wrapped value. Returns `None` if the cell was
    /// uninitialized.
    pub fn into_inner(mut self) -> Option<T> {
        self.take()
    }
}

impl<T> Default for OnceLock<T> {
    /// Creates a new uninitialized cell.
    #[track_caller]
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Debug + 'static> Debug for OnceLock<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_tuple("OnceLock");
        match self.get() {
            Some(value) => d.field(value),
            None => d.field(&format_args!("<uninit>")),
        };
        d.finish()
    }
}

// Safety: the same bounds std's `OnceLock` carries. The value is reached only through `&self` or
// `&mut self`, so sharing the cell shares the value, and initializing it through a shared
// reference sends the value to whichever task reads it next.
unsafe impl<T: Sync + Send> Sync for OnceLock<T> {}
unsafe impl<T: Send> Send for OnceLock<T> {}

impl<T: RefUnwindSafe + UnwindSafe> RefUnwindSafe for OnceLock<T> {}
impl<T: UnwindSafe> UnwindSafe for OnceLock<T> {}

/// The per-execution state of a cell: its value, if it has one, the lock that racing initializers
/// contend on, and whether an initializer left it poisoned.
///
/// Each field carries its own interior mutability, rather than the whole state sitting in one
/// `RefCell`, so that the bookkeeping can be written without taking a `&mut` borrow that would
/// cover the value a reader is holding a reference to.
struct CellState<T> {
    /// The value, once an initializer has produced one.
    value: RefCell<Option<T>>,
    /// Racing initializers contend on this lock, so that only one of them runs. [`OnceCore::take`]
    /// replaces it, which both drops its poisoning and lets the cell be initialized again.
    lock: RefCell<Rc<Mutex<()>>>,
    /// Set while an initializer runs, and left set if that initializer unwinds or fails, so that
    /// the cell remembers that an initializer did not finish. Only a task that holds `lock`, or
    /// one that holds a `&mut` borrow of the cell, reads it, and for those it is accurate.
    poisoned: Cell<bool>,
    /// The clock of the task that initialized the value, so that a read which skips the lock still
    /// establishes causality with the initializer, as [`Once`](crate::sync::Once) does.
    clock: RefCell<VectorClock>,
}

/// What a cell does when an initializer left it poisoned.
#[derive(Clone, Copy, Debug)]
pub(super) enum IfPoisoned {
    /// Let the next caller run its own initializer, like [`std::sync::OnceLock`].
    Retry,
    /// Panic with this message, like [`std::sync::LazyLock`], whose poisoning is unrecoverable.
    Panic(&'static str),
}

/// The shared implementation of [`OnceLock`] and [`LazyLock`](crate::sync::LazyLock): a cell
/// holding at most one value, produced by the first initializer that wins a race for it.
///
/// The value lives in the current execution's storage, keyed by `id`, so that a `static` cell is
/// empty again in every execution. The cell itself holds nothing but that key and the signature of
/// the lock it creates in each execution.
pub(super) struct OnceCore<T> {
    /// Unique identifier for this cell, which keys its storage. Assigned on first use, so that the
    /// constructor can be `const`, as [`Once`](crate::sync::Once) does.
    id: StdLazyLock<usize>,
    /// Signature of the lock this cell creates in each execution.
    signature: ResourceSignature,
    /// The cell behaves as if it owned a `T`, which it does by way of its storage.
    _p: PhantomData<T>,
}

impl<T> OnceCore<T> {
    /// Creates a new empty cell whose lock reports as `resource_type`.
    #[track_caller]
    pub(super) const fn new(resource_type: ResourceType) -> Self {
        Self {
            id: StdLazyLock::new(|| {
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                assert_ne!(id, 0, "id overflow");
                id
            }),
            signature: ResourceSignature::new_const(resource_type),
            _p: PhantomData,
        }
    }

    fn id(&self) -> usize {
        *self.id
    }

    /// The signature of the lock this cell creates in each execution.
    #[cfg(test)]
    pub(super) fn signature(&self) -> &ResourceSignature {
        &self.signature
    }
}

impl<T: 'static> OnceCore<T> {
    /// Returns a pointer to this cell's state in the current execution, or `None` if the cell has
    /// not been touched in this execution yet.
    ///
    /// The state lives in the execution's storage, which holds it at a stable address until the
    /// execution ends, so the pointer stays valid for the rest of the execution. Returning a
    /// pointer rather than a reference keeps the borrow of the `ExecutionState` from outliving the
    /// lookup, so the caller can still update its clock.
    fn state(&self, execution: &ExecutionState) -> Option<*const CellState<T>> {
        execution
            .get_storage::<_, CellState<T>>(self)
            .map(|state| state as *const _)
    }

    /// Like [`Self::state`], but creates this execution's state if the cell does not have one yet.
    fn init_state(&self, execution: &mut ExecutionState) -> *const CellState<T> {
        if self.state(execution).is_none() {
            let state = CellState {
                value: RefCell::new(None),
                lock: RefCell::new(Rc::new(Mutex::new_internal((), self.signature.clone()))),
                poisoned: Cell::new(false),
                clock: RefCell::new(VectorClock::new()),
            };
            execution.init_storage::<_, CellState<T>>(self, state);
        }
        self.state(execution).expect("just initialized")
    }

    /// Returns the cell's value, if it has one, and takes causality from the task that initialized
    /// it.
    ///
    /// Not a scheduling point, like [`Once::is_completed`](crate::sync::Once::is_completed).
    pub(super) fn get(&self) -> Option<&T> {
        ExecutionState::with(|execution| {
            let state = self.state(execution)?;
            // Safety: the state stays at this address until the execution ends.
            let state = unsafe { &*state };
            // Safety: a stored value is never overwritten, and is moved out only through a `&mut`
            // borrow of the cell, which cannot exist while the caller's borrow of `self` does, so
            // nothing takes a `&mut` to the value while this reference is alive. The reference
            // outlives the execution if the cell is a `static`, which is the same promotion
            // `shuttle::lazy_static!` makes.
            let value = unsafe { &*state.value.as_ptr() }.as_ref()?;
            let clock = state.clock.borrow().clone();
            execution.update_clock(&clock);
            Some(value)
        })
    }

    /// Returns a mutable reference to the cell's value, if it has one.
    ///
    /// Not a scheduling point: the `&mut` borrow rules out another task reaching the cell.
    pub(super) fn get_mut(&mut self) -> Option<&mut T> {
        ExecutionState::with(|execution| {
            let state = self.state(execution)?;
            // Safety: as in `get`, and the `&mut` borrow of the cell rules out any other
            // reference to the value.
            unsafe { &mut *(*state).value.as_ptr() }.as_mut()
        })
    }

    /// Whether an initializer left the cell poisoned.
    ///
    /// Accurate only for a task that holds the cell's lock, or that holds a `&mut` borrow of the
    /// cell: an initializer poisons the cell before it runs and clears the flag once it has
    /// stored its value, both while holding the lock.
    pub(super) fn is_poisoned(&self) -> bool {
        ExecutionState::with(|execution| match self.state(execution) {
            // Safety: see `get`.
            Some(state) => unsafe { &*state }.poisoned.get(),
            None => false,
        })
    }

    fn set_poisoned(&self, poisoned: bool) {
        ExecutionState::with(|execution| {
            let state = self.init_state(execution);
            // Safety: see `get`.
            unsafe { &*state }.poisoned.set(poisoned);
        });
    }

    /// Returns the cell's value, initializing it with `f` if it has none.
    pub(super) fn get_or_try_init<F, E>(&self, if_poisoned: IfPoisoned, f: F) -> Result<&T, E>
    where
        F: FnOnce() -> Result<T, E>,
    {
        // A cell that already has a value is read without taking the lock, so a hit after
        // initialization is not a scheduling point. `Once::call_once` does the same.
        if let Some(value) = self.get() {
            return Ok(value);
        }

        let lock = ExecutionState::with(|execution| {
            let state = self.init_state(execution);
            // Safety: see `get`.
            Rc::clone(&unsafe { &*state }.lock.borrow())
        });

        // Acquiring the lock is the scheduling point before the initialization check, where
        // `Once::call_once` takes one too. Whether a poisoned cell can be initialized again is
        // `if_poisoned`'s business, decided from the state's own flag below, so the lock's
        // poisoning is ignored here and the lock is taken as if it were not poisoned.
        let _guard = match lock.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };

        // Another task may have initialized the cell while we waited for the lock.
        if let Some(value) = self.get() {
            return Ok(value);
        }

        if self.is_poisoned() {
            match if_poisoned {
                IfPoisoned::Retry => (),
                IfPoisoned::Panic(message) => panic!("{message}"),
            }
        }

        trace!("won the initialization race for cell {:p}", self);

        // Poison the cell for as long as the initializer runs, so that it stays poisoned if the
        // initializer unwinds. Only a task holding the lock reads the flag, and we hold the lock
        // until `publish` has cleared it again. An initializer that returns an error leaves the
        // cell poisoned too, as std's `get_or_try_init` does.
        self.set_poisoned(true);
        let value = f()?;
        Ok(self.publish(value))
    }

    /// Returns a mutable reference to the cell's value, initializing it with `f` if it has none.
    ///
    /// Not a scheduling point: the `&mut` borrow rules out a race, so there is no lock to take.
    pub(super) fn get_mut_or_init<F>(&mut self, if_poisoned: IfPoisoned, f: F) -> &mut T
    where
        F: FnOnce() -> T,
    {
        if self.is_poisoned() {
            match if_poisoned {
                IfPoisoned::Retry => (),
                IfPoisoned::Panic(message) => panic!("{message}"),
            }
        }

        if self.get_mut().is_none() {
            self.set_poisoned(true);
            let value = f();
            self.publish(value);
        }

        self.get_mut().expect("initialized just above")
    }

    /// Stores `value` in the cell, together with the current task's clock, and returns a reference
    /// to it.
    ///
    /// The caller must hold the cell's lock, or a `&mut` borrow of it, and must have found the cell
    /// empty.
    fn publish(&self, value: T) -> &T {
        ExecutionState::with(|execution| {
            // Later reads of the value take this clock, so that they establish causality with us
            // even though they do not take the lock.
            let clock = execution.increment_clock().clone();
            let state = self
                .state(execution)
                .expect("the cell has state by the time it is initialized");
            // Safety: see `get`.
            let state = unsafe { &*state };
            {
                let mut slot = state.value.borrow_mut();
                assert!(slot.is_none(), "the cell was initialized twice");
                *slot = Some(value);
            }
            state.poisoned.set(false);
            *state.clock.borrow_mut() = clock;
            // Safety: see `get`.
            unsafe { &*state.value.as_ptr() }
                .as_ref()
                .expect("the value was just stored")
        })
    }

    /// Takes the cell's value, leaving the cell empty and ready to be initialized again.
    ///
    /// Not a scheduling point: the `&mut` borrow rules out another task reaching the cell.
    pub(super) fn take(&mut self) -> Option<T> {
        ExecutionState::with(|execution| {
            let state = self.state(execution)?;
            // Safety: see `get_mut`.
            let state = unsafe { &*state };
            let value = state.value.borrow_mut().take()?;
            // The rest of the cell goes back to how it starts an execution, so that the next
            // initializer runs and does not find the old lock's poisoning. std's `take` replaces
            // its `Once` for the same reason.
            *state.lock.borrow_mut() = Rc::new(Mutex::new_internal((), self.signature.clone()));
            state.poisoned.set(false);
            *state.clock.borrow_mut() = VectorClock::new();
            Some(value)
        })
    }
}

impl<T> From<&OnceCore<T>> for StorageKey {
    fn from(core: &OnceCore<T>) -> Self {
        StorageKey(core.id(), 0x4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_resource_signature_once_lock() {
        shuttle_schedulers::check_random(
            || {
                let cell1 = OnceLock::<usize>::new();
                let cell2 = OnceLock::<usize>::new();
                assert_ne!(cell1.core.signature, cell2.core.signature);
            },
            1,
        );
    }

    // Two cells must not share a storage slot, however they were created.
    #[test]
    fn unique_storage_keys() {
        shuttle_schedulers::check_random(
            || {
                let cell1 = OnceLock::<usize>::new();
                let cell2 = OnceLock::<usize>::new();
                cell1.set(1).unwrap();
                cell2.set(2).unwrap();
                assert_ne!(StorageKey::from(&cell1.core), StorageKey::from(&cell2.core));
                assert_eq!(cell1.get(), Some(&1));
                assert_eq!(cell2.get(), Some(&2));
            },
            1,
        );
    }
}
