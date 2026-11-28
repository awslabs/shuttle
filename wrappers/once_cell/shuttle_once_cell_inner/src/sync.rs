//! Thread-safe, blocking version of `OnceCell`: Shuttle's models of `once_cell::sync`.
//!
//! [`OnceCell`] is built on Shuttle's `std::sync::OnceLock` model, and [`Lazy`] on [`OnceCell`],
//! as once_cell builds its `Lazy` on its `OnceCell`. See the [crate docs](crate) for how they
//! differ from once_cell's.

use shuttle::sync::OnceLock;
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::storage::StorageKey;
use std::cell::Cell;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::LazyLock as StdLazyLock;
use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering};

/// What accessing a poisoned `Lazy` panics with, as in once_cell.
const POISONED: &str = "Lazy instance has previously been poisoned";

/// A thread-safe cell which can be written to only once.
///
/// `OnceCell` provides `&` references to the contents without RAII guards.
///
/// Reading a non-`None` value out of `OnceCell` establishes a happens-before relationship with a
/// corresponding write. For example, if task A initializes the cell with `get_or_init(f)`, and
/// task B subsequently reads the result of this call, B also observes all the side effects of `f`.
///
/// # Shuttle
///
/// A value stored by `set` or an initializer lives in the running execution's storage, so a
/// `static` cell is empty again in every execution. A cell created with a value (by
/// [`with_value`](Self::with_value), [`From`] or [`Clone`]) holds it itself, so it has it in every
/// execution. Initializing an empty cell takes two scheduling points; nothing else does. See the
/// [crate docs](crate) for the rest.
///
/// # Example
/// ```
/// use shuttle_once_cell_inner::sync::OnceCell;
///
/// static CELL: OnceCell<String> = OnceCell::new();
///
/// shuttle::check_dfs(
///     || {
///         assert!(CELL.get().is_none());
///
///         shuttle::thread::spawn(|| {
///             let value: &String = CELL.get_or_init(|| "Hello, World!".to_string());
///             assert_eq!(value, "Hello, World!");
///         })
///         .join()
///         .unwrap();
///
///         let value: Option<&String> = CELL.get();
///         assert!(value.is_some());
///         assert_eq!(value.unwrap().as_str(), "Hello, World!");
///     },
///     None,
/// );
/// ```
pub struct OnceCell<T> {
    /// The value the cell was created with, which is the same in every execution. A `&mut` borrow
    /// of the cell can take it out, after which the cell is empty.
    initial: Option<T>,
    /// The value stored in the running execution, if `initial` is `None`.
    cell: OnceLock<T>,
}

impl<T> OnceCell<T> {
    /// Creates a new empty cell.
    #[must_use]
    #[track_caller]
    pub const fn new() -> OnceCell<T> {
        OnceCell {
            initial: None,
            cell: OnceLock::new(),
        }
    }

    /// Creates a new initialized cell.
    ///
    /// The cell holds `value` itself, so it has the value in every execution, and it can be read
    /// outside of one too.
    #[must_use]
    #[track_caller]
    pub const fn with_value(value: T) -> OnceCell<T> {
        OnceCell {
            initial: Some(value),
            cell: OnceLock::new(),
        }
    }
}

impl<T: 'static> OnceCell<T> {
    /// Gets the reference to the underlying value.
    ///
    /// Returns `None` if the cell is empty, or being initialized. This method never blocks, and is
    /// not a scheduling point.
    pub fn get(&self) -> Option<&T> {
        match &self.initial {
            Some(value) => Some(value),
            None => self.cell.get(),
        }
    }

    /// Gets the reference to the underlying value, blocking the current task until it is set.
    ///
    /// Not a scheduling point if the cell has a value. Otherwise the task blocks until another one
    /// stores a value; an initializer that panics or fails does not wake it. If no task is left
    /// that could initialize the cell, Shuttle reports a deadlock.
    ///
    /// ```
    /// use shuttle_once_cell_inner::sync::OnceCell;
    /// use std::sync::Arc;
    ///
    /// shuttle::check_dfs(
    ///     || {
    ///         let cell = Arc::new(OnceCell::new());
    ///         let t = shuttle::thread::spawn({
    ///             let cell = Arc::clone(&cell);
    ///             move || cell.set(92).unwrap()
    ///         });
    ///
    ///         // Returns immediately, but might return None.
    ///         let _value_or_none = cell.get();
    ///
    ///         // Will return 92, but might block until the other task does `.set`.
    ///         let value: &u32 = cell.wait();
    ///         assert_eq!(*value, 92);
    ///         t.join().unwrap();
    ///     },
    ///     None,
    /// );
    /// ```
    #[cfg(feature = "std")]
    pub fn wait(&self) -> &T {
        match &self.initial {
            Some(value) => value,
            None => self.cell.wait(),
        }
    }

    /// Gets the mutable reference to the underlying value.
    ///
    /// Returns `None` if the cell is empty. Not a scheduling point.
    ///
    /// This method is allowed to violate the invariant of writing to a `OnceCell` at most once
    /// because it requires `&mut` access to `self`. As with all interior mutability, `&mut` access
    /// permits arbitrary modification.
    #[inline]
    pub fn get_mut(&mut self) -> Option<&mut T> {
        match &mut self.initial {
            Some(value) => Some(value),
            None => self.cell.get_mut(),
        }
    }

    /// Get the reference to the underlying value, without checking if the cell is initialized.
    ///
    /// # Safety
    ///
    /// Caller must ensure that the cell is in initialized state, and that the contents are
    /// acquired by (synchronized to) this thread.
    ///
    /// # Panics
    ///
    /// Under Shuttle the check is made anyway, and this panics if the cell is empty, where
    /// once_cell's behavior is undefined.
    #[inline]
    pub unsafe fn get_unchecked(&self) -> &T {
        self.get()
            .expect("`OnceCell::get_unchecked` was called on an empty cell, which is undefined behavior in once_cell")
    }

    /// Sets the contents of this cell to `value`.
    ///
    /// Returns `Ok(())` if the cell was empty and `Err(value)` if it was full.
    pub fn set(&self, value: T) -> Result<(), T> {
        match self.try_insert(value) {
            Ok(_) => Ok(()),
            Err((_, value)) => Err(value),
        }
    }

    /// Like [`set`](Self::set), but also returns a reference to the final cell value.
    pub fn try_insert(&self, value: T) -> Result<&T, (&T, T)> {
        match &self.initial {
            Some(current) => Err((current, value)),
            None => self.cell.try_insert(value),
        }
    }

    /// Gets the contents of the cell, initializing it with `f` if the cell was empty.
    ///
    /// Many tasks may call `get_or_init` concurrently with different initializing functions, but
    /// it is guaranteed that only one function will be executed.
    ///
    /// # Panics
    ///
    /// If `f` panics, the panic is propagated to the caller, and the cell remains uninitialized.
    ///
    /// It is an error to reentrantly initialize the cell from `f`. once_cell deadlocks; Shuttle
    /// fails the test with a deadlock error.
    pub fn get_or_init<F>(&self, f: F) -> &T
    where
        F: FnOnce() -> T,
    {
        match &self.initial {
            Some(value) => value,
            None => self.cell.get_or_init(f),
        }
    }

    /// Gets the contents of the cell, initializing it with `f` if the cell was empty. If the cell
    /// was empty and `f` failed, an error is returned.
    ///
    /// # Panics
    ///
    /// If `f` panics, the panic is propagated to the caller, and the cell remains uninitialized.
    ///
    /// It is an error to reentrantly initialize the cell from `f`. once_cell deadlocks; Shuttle
    /// fails the test with a deadlock error.
    pub fn get_or_try_init<F, E>(&self, f: F) -> Result<&T, E>
    where
        F: FnOnce() -> Result<T, E>,
    {
        match &self.initial {
            Some(value) => Ok(value),
            None => self.cell.get_or_try_init(f),
        }
    }

    /// Takes the value out of this `OnceCell`, moving it back to an uninitialized state.
    ///
    /// Has no effect and returns `None` if the `OnceCell` hasn't been initialized. Not a
    /// scheduling point.
    pub fn take(&mut self) -> Option<T> {
        self.initial.take().or_else(|| self.cell.take())
    }

    /// Consumes the `OnceCell`, returning the wrapped value. Returns `None` if the cell was empty.
    #[inline]
    pub fn into_inner(self) -> Option<T> {
        let OnceCell { initial, cell } = self;
        initial.or_else(|| cell.into_inner())
    }
}

impl<T> Default for OnceCell<T> {
    #[track_caller]
    fn default() -> OnceCell<T> {
        OnceCell::new()
    }
}

impl<T: fmt::Debug + 'static> fmt::Debug for OnceCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.get() {
            Some(v) => f.debug_tuple("OnceCell").field(v).finish(),
            None => f.write_str("OnceCell(Uninit)"),
        }
    }
}

impl<T: Clone + 'static> Clone for OnceCell<T> {
    #[track_caller]
    fn clone(&self) -> OnceCell<T> {
        match self.get() {
            Some(value) => OnceCell::with_value(value.clone()),
            None => OnceCell::new(),
        }
    }

    fn clone_from(&mut self, source: &Self) {
        match (self.get_mut(), source.get()) {
            (Some(this), Some(source)) => this.clone_from(source),
            _ => *self = source.clone(),
        }
    }
}

impl<T> From<T> for OnceCell<T> {
    #[track_caller]
    fn from(value: T) -> Self {
        OnceCell::with_value(value)
    }
}

impl<T: PartialEq + 'static> PartialEq for OnceCell<T> {
    fn eq(&self, other: &OnceCell<T>) -> bool {
        self.get() == other.get()
    }
}

impl<T: Eq + 'static> Eq for OnceCell<T> {}

/// A value which is initialized on the first access.
///
/// This type is thread-safe and can be used in statics.
///
/// # Shuttle
///
/// A `static` `Lazy` is uninitialized again at the start of every execution, and its initializer
/// runs once per execution. So the initializer is an `F: Fn() -> T`, where once_cell's is an
/// `F: FnOnce() -> T`, and a `Lazy` is `Sync` if `F` is, where once_cell's needs `F: Send`. Forcing
/// an uninitialized `Lazy` takes two scheduling points; nothing else does. If the initializer
/// panics, the `Lazy` is poisoned for the rest of the execution, as in once_cell: every later
/// access panics, with once_cell's message.
///
/// # Example
///
/// ```
/// use std::collections::HashMap;
///
/// use shuttle_once_cell_inner::sync::Lazy;
///
/// static HASHMAP: Lazy<HashMap<i32, String>> = Lazy::new(|| {
///     let mut m = HashMap::new();
///     m.insert(13, "Spica".to_string());
///     m.insert(74, "Hoyten".to_string());
///     m
/// });
///
/// shuttle::check_dfs(
///     || {
///         shuttle::thread::spawn(|| {
///             assert_eq!(HASHMAP.get(&13).map(String::as_str), Some("Spica"));
///         })
///         .join()
///         .unwrap();
///         assert_eq!(HASHMAP.get(&74).map(String::as_str), Some("Hoyten"));
///     },
///     None,
/// );
/// ```
pub struct Lazy<T, F = fn() -> T> {
    cell: OnceCell<T>,
    init: F,
    /// Whether an initializer has started in the running execution. once_cell takes its
    /// initializer out of the `Lazy` to run it, so a `Lazy` that is still empty once it has, was
    /// poisoned by an initializer that panicked. This flag stands for that, per execution.
    started: Started,
}

impl<T: fmt::Debug + 'static, F> fmt::Debug for Lazy<T, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lazy")
            .field("cell", &self.cell)
            .field("init", &"..")
            .finish()
    }
}

impl<T, F> Lazy<T, F> {
    /// Creates a new lazy value with the given initializing function.
    #[track_caller]
    pub const fn new(f: F) -> Lazy<T, F> {
        Lazy {
            cell: OnceCell::new(),
            init: f,
            started: Started::new(),
        }
    }
}

impl<T: 'static, F> Lazy<T, F> {
    /// Consumes this `Lazy` returning the stored value.
    ///
    /// Returns `Ok(value)` if `Lazy` is initialized and `Err(f)` otherwise.
    ///
    /// # Panics
    ///
    /// Panics if the `Lazy` is poisoned.
    pub fn into_value(this: Lazy<T, F>) -> Result<T, F> {
        let Lazy { cell, init, started } = this;
        match cell.into_inner() {
            Some(value) => Ok(value),
            None if started.get() => panic!("{POISONED}"),
            None => Err(init),
        }
    }
}

impl<T: 'static, F: Fn() -> T> Lazy<T, F> {
    /// Forces the evaluation of this lazy value and returns a reference to the result. This is
    /// equivalent to the `Deref` impl, but is explicit.
    ///
    /// Blocks the calling task if another one is running the initializer.
    ///
    /// # Panics
    ///
    /// If the initializer panics, the panic is propagated to the caller, and the `Lazy` is
    /// poisoned: every later access to it panics.
    pub fn force(this: &Lazy<T, F>) -> &T {
        this.cell.get_or_init(|| {
            // This runs with the cell's lock held, so only one task gets here at a time.
            if this.started.replace(true) {
                panic!("{POISONED}");
            }
            (this.init)()
        })
    }

    /// Forces the evaluation of this lazy value and returns a mutable reference to the result.
    /// This is equivalent to the `DerefMut` impl, but is explicit.
    ///
    /// Not a scheduling point: the `&mut` borrow rules out a race.
    ///
    /// # Panics
    ///
    /// If the initializer panics, the panic is propagated to the caller, and the `Lazy` is
    /// poisoned. Panics if the `Lazy` is already poisoned.
    pub fn force_mut(this: &mut Lazy<T, F>) -> &mut T {
        if this.cell.get_mut().is_none() {
            if this.started.replace(true) {
                panic!("{POISONED}");
            }
            let value = (this.init)();
            this.cell = OnceCell::with_value(value);
        }
        this.cell.get_mut().unwrap_or_else(|| unreachable!())
    }

    /// Gets the reference to the result of this lazy value if it was initialized, otherwise
    /// returns `None`.
    pub fn get(this: &Lazy<T, F>) -> Option<&T> {
        this.cell.get()
    }

    /// Gets the reference to the result of this lazy value if it was initialized, otherwise
    /// returns `None`.
    pub fn get_mut(this: &mut Lazy<T, F>) -> Option<&mut T> {
        this.cell.get_mut()
    }
}

impl<T: 'static, F: Fn() -> T> Deref for Lazy<T, F> {
    type Target = T;
    fn deref(&self) -> &T {
        Lazy::force(self)
    }
}

impl<T: 'static, F: Fn() -> T> DerefMut for Lazy<T, F> {
    fn deref_mut(&mut self) -> &mut T {
        Lazy::force_mut(self)
    }
}

impl<T: Default> Default for Lazy<T> {
    /// Creates a new lazy value using `Default` as the initializing function.
    #[track_caller]
    fn default() -> Lazy<T> {
        Lazy::new(T::default)
    }
}

/// Source of the identifiers that key [`Started`]'s per-execution storage.
static NEXT_ID: StdAtomicUsize = StdAtomicUsize::new(1);

/// Whether a `Lazy`'s initializer has started in the running execution: a flag in the execution's
/// storage, so that it is clear again in the next one.
///
/// Reading and writing it is not a scheduling point. Only the task that holds the cell's lock, or a
/// `&mut` borrow of the `Lazy`, touches it.
struct Started {
    /// Unique identifier, which keys the storage. Assigned on first use, so that the constructor
    /// can be `const`, as Shuttle's own cells do.
    id: StdLazyLock<usize>,
}

impl Started {
    const fn new() -> Self {
        Self {
            id: StdLazyLock::new(|| {
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                assert_ne!(id, 0, "id overflow");
                id
            }),
        }
    }

    fn get(&self) -> bool {
        ExecutionState::with(|execution| execution.get_storage::<_, Cell<bool>>(self).is_some_and(Cell::get))
    }

    /// Sets the flag to `value`, returning what it was.
    fn replace(&self, value: bool) -> bool {
        ExecutionState::with(|execution| {
            if execution.get_storage::<_, Cell<bool>>(self).is_none() {
                execution.init_storage::<_, Cell<bool>>(self, Cell::new(false));
            }
            execution
                .get_storage::<_, Cell<bool>>(self)
                .expect("just initialized")
                .replace(value)
        })
    }
}

impl From<&Started> for StorageKey {
    fn from(started: &Started) -> Self {
        // `0x6f6e_6365` ("once") keeps these slots apart from those of Shuttle's own primitives.
        StorageKey(*started.id, 0x6f6e_6365)
    }
}

/// ```compile_fail
/// struct S(*mut ());
/// unsafe impl Sync for S {}
///
/// fn share<T: Sync>(_: &T) {}
/// share(&shuttle_once_cell_inner::sync::OnceCell::<S>::new());
/// ```
///
/// ```compile_fail
/// struct S(*mut ());
/// unsafe impl Sync for S {}
///
/// fn share<T: Sync>(_: &T) {}
/// share(&shuttle_once_cell_inner::sync::Lazy::<S>::new(|| unimplemented!()));
/// ```
///
/// A `Lazy` whose initializer is not `Sync` is not `Sync` either, unlike once_cell's: Shuttle runs
/// the initializer once per execution, through a shared reference, and executions of tests that run
/// in parallel run on different threads.
///
/// ```compile_fail
/// use std::cell::Cell;
///
/// fn share<T: Sync>(_: &T) {}
/// let counter = Cell::new(0);
/// let lazy: shuttle_once_cell_inner::sync::Lazy<i32, _> =
///     shuttle_once_cell_inner::sync::Lazy::new(move || counter.get());
/// share(&lazy);
/// ```
fn _dummy() {}
