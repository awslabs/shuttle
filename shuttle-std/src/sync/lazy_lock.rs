use crate::sync::once_lock::{IfPoisoned, OnceCore};
use crate::sync::ResourceType;
use std::convert::Infallible;
use std::fmt::Debug;
use std::ops::{Deref, DerefMut};
use std::panic::{RefUnwindSafe, UnwindSafe};

/// What accessing a poisoned `LazyLock` panics with, as in std.
const POISONED: &str = "LazyLock instance has previously been poisoned";

/// A value which is initialized on the first access, the same as [`std::sync::LazyLock`].
///
/// # Poisoning
///
/// If the initializer panics, the lock is poisoned, and every later access to it, by a dereference
/// or by [`force`](LazyLock::force), panics as well. As in std, this poisoning cannot be recovered
/// from, though it only lasts for the execution in which the initializer panicked.
///
/// # Differences from `std`
///
/// The value lives in the current execution's storage rather than in the lock, so a `static`
/// `LazyLock` is uninitialized again at the start of every execution, and its initializer runs
/// once per execution, as [`shuttle::lazy_static!`] does. A value that carried over would make an
/// execution depend on the ones before it, and so make a failing schedule impossible to replay.
/// The rest of the differences follow from where the value lives:
///
/// * The initializer runs more than once, so it is an `F: Fn() -> T` where std takes an
///   `F: FnOnce() -> T`. A closure that can only be called once has to be turned into one that
///   can be called repeatedly, for instance by cloning what it captures.
/// * The methods that reach the value require `T: 'static`, which the execution's storage
///   requires.
/// * They have to run inside a Shuttle execution, including those of [`Debug`], which reads the
///   value.
/// * A value is dropped when the execution ends, not when the lock is dropped, so a `LazyLock`
///   that goes out of scope part way through an execution keeps its value until the end of it.
/// * There is no `From<T>` impl, because a lock built from a value has no initializer to run in
///   the next execution.
/// * A `LazyLock` is `Sync` only if `F` is `Sync` too, because it calls the initializer through a
///   shared reference, and the executions of tests that run in parallel run on different threads.
///   Every `fn() -> T`, and every closure that captures only `Sync` values, is `Sync`.
///
/// ```compile_fail
/// use std::cell::Cell;
///
/// fn share<T: Sync>(_: &T) {}
/// let counter = Cell::new(0);
/// let lazy: shuttle_std::sync::LazyLock<i32, _> =
///     shuttle_std::sync::LazyLock::new(move || counter.get());
/// share(&lazy);
/// ```
///
/// [`shuttle::lazy_static!`]: https://docs.rs/shuttle/*/shuttle/macro.lazy_static.html
pub struct LazyLock<T, F = fn() -> T> {
    cell: OnceCore<T>,
    init: F,
}

impl<T, F: Fn() -> T> LazyLock<T, F> {
    /// Creates a new lazy value with the given initializing function.
    #[track_caller]
    pub const fn new(f: F) -> Self {
        Self {
            cell: OnceCore::new(ResourceType::LazyLock),
            init: f,
        }
    }
}

impl<T: 'static, F: Fn() -> T> LazyLock<T, F> {
    /// Forces the evaluation of this lazy value and returns a reference to the result.
    ///
    /// This is equivalent to the `Deref` impl, but is explicit.
    ///
    /// This method will block the calling task if another initialization routine is currently
    /// running.
    ///
    /// # Panics
    ///
    /// If the initializer panics, the panic is propagated to the caller and the lock becomes
    /// poisoned, so every later access to it panics too.
    pub fn force(this: &LazyLock<T, F>) -> &T {
        match this
            .cell
            .get_or_try_init(IfPoisoned::Panic(POISONED), || Ok::<T, Infallible>((this.init)()))
        {
            Ok(value) => value,
            Err(infallible) => match infallible {},
        }
    }

    /// Forces the evaluation of this lazy value and returns a mutable reference to the result.
    ///
    /// This method never blocks, and is not a scheduling point: the `&mut` borrow rules out
    /// another task racing to initialize the lock.
    ///
    /// # Panics
    ///
    /// If the initializer panics, the panic is propagated to the caller and the lock becomes
    /// poisoned, so every later access to it panics too. Panics if the lock is already poisoned.
    pub fn force_mut(this: &mut LazyLock<T, F>) -> &mut T {
        let Self { cell, init } = this;
        cell.get_mut_or_init(IfPoisoned::Panic(POISONED), init)
    }

    /// Consumes this `LazyLock` returning the stored value.
    ///
    /// Returns `Ok(value)` if `Lazy` is initialized and `Err(f)` otherwise.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn into_inner(mut this: Self) -> Result<T, F> {
        if this.cell.is_poisoned() {
            panic!("{POISONED}");
        }
        match this.cell.take() {
            Some(value) => Ok(value),
            None => Err(this.init),
        }
    }
}

impl<T: 'static, F> LazyLock<T, F> {
    /// Returns a reference to the value if initialized. Otherwise (if uninitialized or poisoned),
    /// returns `None`.
    ///
    /// This method never blocks and is not a scheduling point.
    pub fn get(this: &LazyLock<T, F>) -> Option<&T> {
        this.cell.get()
    }

    /// Returns a mutable reference to the value if initialized. Otherwise (if uninitialized or
    /// poisoned), returns `None`.
    ///
    /// This method never blocks and is not a scheduling point.
    pub fn get_mut(this: &mut LazyLock<T, F>) -> Option<&mut T> {
        this.cell.get_mut()
    }
}

impl<T: 'static, F: Fn() -> T> Deref for LazyLock<T, F> {
    type Target = T;

    /// Dereferences the value.
    ///
    /// This method will block the calling task if another initialization routine is currently
    /// running.
    ///
    /// # Panics
    ///
    /// If the initializer panics, the panic is propagated to the caller and the lock becomes
    /// poisoned, so every later access to it panics too.
    fn deref(&self) -> &T {
        LazyLock::force(self)
    }
}

impl<T: 'static, F: Fn() -> T> DerefMut for LazyLock<T, F> {
    /// # Panics
    ///
    /// If the initializer panics, the panic is propagated to the caller and the lock becomes
    /// poisoned, so every later access to it panics too. Panics if the lock is already poisoned.
    fn deref_mut(&mut self) -> &mut T {
        LazyLock::force_mut(self)
    }
}

impl<T: Default> Default for LazyLock<T> {
    /// Creates a new lazy value using `Default` as the initializing function.
    #[track_caller]
    fn default() -> Self {
        Self::new(T::default)
    }
}

impl<T: Debug + 'static, F> Debug for LazyLock<T, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_tuple("LazyLock");
        match LazyLock::get(self) {
            Some(value) => d.field(value),
            None => d.field(&format_args!("<uninit>")),
        };
        d.finish()
    }
}

// Safety: std's bounds, and `F: Sync`. std needs no `F: Sync` because it never creates a `&F` from
// a `&LazyLock<T, F>`. This implementation does, to run the initializer again in each execution,
// and while the tasks of one execution share a thread, the executions of tests that run in parallel
// do not: two of them that share a `LazyLock` call `F` through a shared reference at the same time.
unsafe impl<T: Sync + Send, F: Send + Sync> Sync for LazyLock<T, F> {}

impl<T: RefUnwindSafe + UnwindSafe, F: UnwindSafe> RefUnwindSafe for LazyLock<T, F> {}
impl<T: UnwindSafe, F: UnwindSafe> UnwindSafe for LazyLock<T, F> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_resource_signature_lazy_lock() {
        shuttle_schedulers::check_random(
            || {
                let lazy1 = LazyLock::new(|| 0usize);
                let lazy2 = LazyLock::new(|| 0usize);
                assert_ne!(lazy1.cell.signature(), lazy2.cell.signature());
            },
            1,
        );
    }
}
