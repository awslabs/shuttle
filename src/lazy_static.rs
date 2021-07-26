//! Shuttle's implementation of the [`lazy_static`] crate, v1.4.0.
//!
//! Using this structure, it is possible to have `static`s that require code to be executed at
//! runtime in order to be initialized. Lazy statics should be created with the
//! [`lazy_static!`](crate::lazy_static!) macro.
//!
//! [`lazy_static`]: https://crates.io/crates/lazy_static

use crate::runtime::execution::ExecutionState;
use crate::runtime::storage::StorageKey;
use crate::sync::Once;
use std::marker::PhantomData;

/// Shuttle's implementation of `lazy_static::Lazy` (aka the unstable `std::lazy::Lazy`).
// Sadly, the fields of this thing need to be public because function pointers in const fns are
// unstable, so an explicit instantiation is the only way to construct this struct. User code should
// not rely on these fields.
pub struct Lazy<T: Sync> {
    #[doc(hidden)]
    pub cell: Once,
    #[doc(hidden)]
    pub init: fn() -> T,
    #[doc(hidden)]
    pub _p: PhantomData<T>,
}

impl<T: Sync> std::fmt::Debug for Lazy<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lazy").finish_non_exhaustive()
    }
}

impl<T: Sync> Lazy<T> {
    /// Get a reference to the lazy value, initializing it first if necessary.
    pub fn get(&'static self) -> &T {
        // Safety: see the usage below
        unsafe fn extend_lt<'a, T>(t: &'a T) -> &'static T {
            std::mem::transmute(t)
        }

        // We implement lazy statics by using a `Once` to mediate initialization of the static, just
        // as the real implementation does. If two threads race on initializing the static, they
        // will race on the `Once::call_once`, and only one of them will initialize the static. Once
        // it's initialized, all future accesses can bypass the `Once` entirely and just access the
        // storage cell.

        let initialize = ExecutionState::with(|state| state.get_storage::<_, T>(self).is_none());

        if initialize {
            // There's not yet a value for this static, so try to initialize it (possibly racing)
            self.cell.call_once(|| {
                let value = (self.init)();
                ExecutionState::with(|state| state.init_storage(self, value));
            });
        }

        // At this point we're guaranteed that a value exists for this static, so read it
        ExecutionState::with(|state| {
            let value = state.get_storage(self).expect("should be initialized");
            // Safety: this *isn't* safe. We are promoting to a `'static` lifetime here, but this
            // object does not actually live that long. It would be possible for this reference to
            // escape the client code and be used after it becomes invalid when the execution ends.
            // But there's not really any way around this -- the semantics of static values and
            // Shuttle executions are incompatible.
            //
            // In reality, this should be safe because any code that uses a Shuttle `lazy_static`
            // probably uses a regular `lazy_static` in its non-test version, and if that version
            // compiles then the Shuttle version is also safe. We choose this unsafe path because
            // it's intended to be used only in testing code, which we want to remain as compatible
            // with real-world code as possible and so needs to preserve the semantics of statics.
            //
            // See also https://github.com/tokio-rs/loom/pull/125
            unsafe { extend_lt(value) }
        })
    }
}

impl<T: Sync> From<&Lazy<T>> for StorageKey {
    fn from(lazy: &Lazy<T>) -> Self {
        StorageKey(lazy as *const _ as usize, 0x3)
    }
}
