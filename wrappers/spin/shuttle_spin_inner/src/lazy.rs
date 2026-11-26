//! Synchronization primitives for lazy evaluation.

use crate::{RelaxStrategy, Spin, once::Once};
use core::{cell::Cell, fmt, marker::PhantomData, ops::Deref};

/// A value which is initialized on the first access.
///
/// This type is a thread-safe `Lazy`, and can be used in statics.
///
/// # Under Shuttle
///
/// The value lives in the running execution, as a [`Once`]'s does, so a `static` `Lazy` runs its
/// initializer again in every execution, as `shuttle::lazy_static!` does, and its value is dropped
/// at the end of the execution. That needs the initializer to be callable more than once, so
/// [`force`](Self::force) and `Deref` require `F: Fn() -> T` where spin's require `F: FnOnce() -> T`.
/// The default `fn() -> T`, and every closure that captures nothing, satisfy it. A `Lazy` is `Sync`
/// only if `F` is too, as every execution calls the initializer through a shared reference, and the
/// value must be `'static`, as a `Once`'s must be to be stored. The scheduling points are those of
/// [`Once::call_once`].
pub struct Lazy<T, F = fn() -> T, R = Spin> {
    cell: Once<T, R>,
    init: F,
    // spin keeps the initializer in a `Cell`, which decides the auto traits: `Sync` only through the
    // impl below.
    _cell: PhantomData<Cell<()>>,
}

impl<T: fmt::Debug, F, R> fmt::Debug for Lazy<T, F, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_tuple("Lazy");
        let d = if let Some(x) = self.cell.get() {
            d.field(&x)
        } else {
            d.field(&format_args!("<uninit>"))
        };
        d.finish()
    }
}

// spin's impl, for its default relax strategy, with `F: Sync` added: every execution calls `init`
// through a shared reference.
unsafe impl<T, F: Send + Sync> Sync for Lazy<T, F> where Once<T>: Sync {}
// auto-derived `Send` impl is OK.

impl<T, F, R> Lazy<T, F, R> {
    /// Creates a new lazy value with the given initializing function.
    pub const fn new(f: F) -> Self {
        Self {
            cell: Once::new(),
            init: f,
            _cell: PhantomData,
        }
    }

    /// Retrieves a mutable pointer to the inner data.
    ///
    /// This is especially useful when interfacing with low level code or FFI where the caller
    /// explicitly knows that it has exclusive access to the inner data. Note that reading from
    /// this pointer is UB until initialized or directly written to.
    pub fn as_mut_ptr(&self) -> *mut T {
        self.cell.as_mut_ptr()
    }
}

impl<T: 'static, F: Fn() -> T, R: RelaxStrategy> Lazy<T, F, R> {
    /// Forces the evaluation of this lazy value and returns a reference to result. This is
    /// equivalent to the `Deref` impl, but is explicit.
    pub fn force(this: &Self) -> &T {
        this.cell.call_once(|| (this.init)())
    }
}

impl<T: 'static, F: Fn() -> T, R: RelaxStrategy> Deref for Lazy<T, F, R> {
    type Target = T;

    fn deref(&self) -> &T {
        Self::force(self)
    }
}

impl<T: Default, R> Default for Lazy<T, fn() -> T, R> {
    /// Creates a new lazy value using `Default` as the initializing function.
    fn default() -> Self {
        Self::new(T::default)
    }
}
