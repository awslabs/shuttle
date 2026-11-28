use super::UnsafeCell;
use std::fmt;
use std::ops::{Deref, DerefMut};

enum State<T, F> {
    Uninit(F),
    Init(T),
    Poisoned,
}

/// A version of [`std::cell::LazyCell`] that keeps its state in an [`UnsafeCell`], so that each
/// access to the state goes through a [`Ptr`](super::Ptr).
///
/// A reference that [`force`](Self::force) returns is taken from the cell with one read. Unlike
/// std's `LazyCell`, `new` isn't `const`.
pub struct LazyCell<T, F = fn() -> T> {
    state: UnsafeCell<State<T, F>>,
}

impl<T, F: FnOnce() -> T> LazyCell<T, F> {
    /// Makes a new cell, whose value `f` makes when it is first needed.
    pub fn new(f: F) -> Self {
        Self {
            state: UnsafeCell::new(State::Uninit(f)),
        }
    }

    /// The value, which the function makes first if it hasn't yet.
    ///
    /// # Panics
    ///
    /// If the function panics, the panic goes on to the caller, and the cell is poisoned: every
    /// later access to the value panics. A call to `force` from the function panics the same way.
    pub fn force(this: &LazyCell<T, F>) -> &T {
        // SAFETY: a `LazyCell` isn't `Sync`. Once the state is `Init`, only methods that take
        // `&mut self` change it, so the value stays valid while `this` lives.
        let state = this.state.with(|state| unsafe { &*state });
        match state {
            State::Init(value) => value,
            State::Uninit(_) => Self::really_init(this),
            State::Poisoned => panic_poisoned(),
        }
    }

    #[cold]
    fn really_init(this: &LazyCell<T, F>) -> &T {
        let state = this.state.get();
        // SAFETY: a `LazyCell` isn't `Sync`, and the state isn't `Init`, so there is no reference
        // to a value in it. The state stays `Poisoned` if `f` panics.
        let f = match unsafe { state.replace(State::Poisoned) } {
            State::Uninit(f) => f,
            _ => unreachable!("the state was `Uninit`"),
        };
        let value = f();
        // SAFETY: as above.
        unsafe { state.write(State::Init(value)) };
        match LazyCell::get(this) {
            Some(value) => value,
            None => unreachable!("the value was just made"),
        }
    }

    /// A mutable reference to the value, which the function makes first if it hasn't yet.
    ///
    /// # Panics
    ///
    /// As for [`force`](Self::force).
    pub fn force_mut(this: &mut LazyCell<T, F>) -> &mut T {
        let state = this.state.get_mut();
        if let State::Uninit(_) = state {
            let State::Uninit(f) = std::mem::replace(state, State::Poisoned) else {
                unreachable!("the state was `Uninit`")
            };
            *state = State::Init(f());
        }
        match state {
            State::Init(value) => value,
            State::Poisoned => panic_poisoned(),
            State::Uninit(_) => unreachable!("the value was just made"),
        }
    }
}

impl<T, F> LazyCell<T, F> {
    /// A reference to the value, or `None` if it hasn't been made yet.
    pub fn get(this: &LazyCell<T, F>) -> Option<&T> {
        // SAFETY: as in `force`.
        this.state.with(|state| match unsafe { &*state } {
            State::Init(value) => Some(value),
            _ => None,
        })
    }

    /// A mutable reference to the value, or `None` if it hasn't been made yet.
    pub fn get_mut(this: &mut LazyCell<T, F>) -> Option<&mut T> {
        match this.state.get_mut() {
            State::Init(value) => Some(value),
            _ => None,
        }
    }
}

impl<T, F: FnOnce() -> T> Deref for LazyCell<T, F> {
    type Target = T;

    fn deref(&self) -> &T {
        LazyCell::force(self)
    }
}

impl<T, F: FnOnce() -> T> DerefMut for LazyCell<T, F> {
    fn deref_mut(&mut self) -> &mut T {
        LazyCell::force_mut(self)
    }
}

impl<T: Default> Default for LazyCell<T> {
    fn default() -> Self {
        Self::new(T::default)
    }
}

impl<T: fmt::Debug, F> fmt::Debug for LazyCell<T, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_tuple("LazyCell");
        match LazyCell::get(self) {
            Some(value) => d.field(value),
            None => d.field(&format_args!("<uninit>")),
        };
        d.finish()
    }
}

impl<T, F> From<T> for LazyCell<T, F> {
    fn from(value: T) -> Self {
        Self {
            state: UnsafeCell::new(State::Init(value)),
        }
    }
}

#[cold]
fn panic_poisoned() -> ! {
    panic!("LazyCell instance has previously been poisoned")
}
