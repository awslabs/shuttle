use super::UnsafeCell;
use std::fmt;

/// A version of [`std::cell::OnceCell`] that keeps its value in an [`UnsafeCell`], so that each
/// access to the value goes through a [`Ptr`](super::Ptr).
///
/// A reference that [`get`](Self::get) returns is taken from the cell with one read. Unlike std's
/// `OnceCell`, `new` isn't `const`.
pub struct OnceCell<T> {
    inner: UnsafeCell<Option<T>>,
}

impl<T> OnceCell<T> {
    /// Makes a new, empty cell.
    pub fn new() -> Self {
        Self {
            inner: UnsafeCell::new(None),
        }
    }

    /// A reference to the value, or `None` if the cell is empty.
    pub fn get(&self) -> Option<&T> {
        // SAFETY: a `OnceCell` isn't `Sync`, and once it holds a value, only methods that take
        // `&mut self` change it. So the value stays valid while `&self` lives.
        self.inner.with(|value| unsafe { (*value).as_ref() })
    }

    /// A mutable reference to the value, or `None` if the cell is empty.
    pub fn get_mut(&mut self) -> Option<&mut T> {
        self.inner.get_mut().as_mut()
    }

    /// Sets the value if the cell is empty, or returns `value` if it isn't.
    pub fn set(&self, value: T) -> Result<(), T> {
        match self.try_insert(value) {
            Ok(_) => Ok(()),
            Err((_, value)) => Err(value),
        }
    }

    fn try_insert(&self, value: T) -> Result<&T, (&T, T)> {
        if let Some(old) = self.get() {
            return Err((old, value));
        }
        // SAFETY: the cell is empty, so there is no reference to a value in it, and a `OnceCell`
        // isn't `Sync`.
        unsafe { self.inner.get().write(Some(value)) };
        match self.get() {
            Some(value) => Ok(value),
            None => unreachable!("the cell was just set"),
        }
    }

    /// The value, which `f` makes first if the cell is empty.
    ///
    /// # Panics
    ///
    /// If `f` panics, the panic goes on to the caller, and the cell stays empty. If `f` sets the
    /// cell, as a call to `get_or_init` from `f` does, this panics with "reentrant init", as std's
    /// `OnceCell` does.
    pub fn get_or_init<F>(&self, f: F) -> &T
    where
        F: FnOnce() -> T,
    {
        if let Some(value) = self.get() {
            return value;
        }
        match self.try_insert(f()) {
            Ok(value) => value,
            Err(_) => panic!("reentrant init"),
        }
    }

    /// Unwraps the value, or returns `None` if the cell is empty.
    pub fn into_inner(self) -> Option<T> {
        self.inner.into_inner()
    }

    /// Takes the value out, and leaves the cell empty.
    pub fn take(&mut self) -> Option<T> {
        std::mem::take(self).into_inner()
    }
}

impl<T> Default for OnceCell<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: fmt::Debug> fmt::Debug for OnceCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_tuple("OnceCell");
        match self.get() {
            Some(value) => d.field(value),
            None => d.field(&format_args!("<uninit>")),
        };
        d.finish()
    }
}

impl<T: Clone> Clone for OnceCell<T> {
    fn clone(&self) -> Self {
        let cell = Self::new();
        if let Some(value) = self.get() {
            let _ = cell.set(value.clone());
        }
        cell
    }
}

impl<T: PartialEq> PartialEq for OnceCell<T> {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl<T: Eq> Eq for OnceCell<T> {}

impl<T> From<T> for OnceCell<T> {
    fn from(value: T) -> Self {
        Self {
            inner: UnsafeCell::new(Some(value)),
        }
    }
}
