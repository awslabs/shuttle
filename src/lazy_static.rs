//! Shuttle's implementation of the [`lazy_static`] crate, v1.4.0.
//!
//! Using this structure, it is possible to have `static`s that require code to be executed at
//! runtime in order to be initialized. Lazy statics should be created with the
//! [`lazy_static!`](crate::lazy_static!) macro.
//!
//! # Warning about drop behavior
//!
//! Shuttle's implementation of `lazy_static` will drop the static value at the end of an execution,
//! and so run the value's [`Drop`] implementation. The actual `lazy_static` crate does not drop the
//! static values, so this difference may cause false positives.
//!
//! To disable the warning printed about this issue, set the `SHUTTLE_SILENCE_WARNINGS` environment
//! variable to any value, or set the [`silence_warnings`](crate::Config::silence_warnings) field of
//! [`Config`](crate::Config) to true.
//!
//! [`lazy_static`]: https://crates.io/crates/lazy_static

use crate::runtime::execution::ExecutionState;
use crate::runtime::storage::StorageKey;
use crate::sync::Once;
use std::marker::PhantomData;

// `use lazy_static::lazy_static;` is valid, thus `use shuttle::lazy_static::lazy_static;` should be as well.
pub use crate::lazy_static;

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
        unsafe fn extend_lt<T>(t: &T) -> &'static T {
            std::mem::transmute(t)
        }

        // We implement lazy statics by using a `Once` to mediate initialization of the static, just
        // as the real implementation does. If two threads race on initializing the static, they
        // will race on the `Once::call_once`, and only one of them will initialize the static. Once
        // it's initialized, all future accesses can bypass the `Once` entirely and just access the
        // storage cell.

        let initialize = ExecutionState::with(|state| state.get_storage::<_, DropGuard<T>>(self).is_none());

        if initialize {
            // There's not yet a value for this static, so try to initialize it (possibly racing)
            self.cell.call_once(|| {
                let value = (self.init)();
                ExecutionState::with(|state| {
                    state.init_storage(self, DropGuard::new(value, state.config.silence_warnings))
                });
            });
        }

        // At this point we're guaranteed that a value exists for this static, so read it
        ExecutionState::with(|state| {
            let drop_guard: &DropGuard<T> = state.get_storage(self).expect("should be initialized");
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
            unsafe { extend_lt(&drop_guard.value) }
        })
    }
}

impl<T: Sync> From<&Lazy<T>> for StorageKey {
    fn from(lazy: &Lazy<T>) -> Self {
        StorageKey(lazy as *const _ as usize, 0x3)
    }
}

/// Support trait for enabling a few common operation on lazy static values.
///
/// This is implemented by each defined lazy static, and used by the free functions in this crate.
pub trait LazyStatic {
    #[doc(hidden)]
    fn initialize(lazy: &Self);
}

/// Takes a shared reference to a lazy static and initializes it if it has not been already.
///
/// This can be used to control the initialization point of a lazy static.
pub fn initialize<T: LazyStatic>(lazy: &T) {
    LazyStatic::initialize(lazy);
}

static PRINTED_DROP_WARNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn maybe_warn_about_drop(silence_warnings: bool) {
    use owo_colors::OwoColorize;
    use std::sync::atomic::Ordering;

    if PRINTED_DROP_WARNING
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        if silence_warnings || std::env::var("SHUTTLE_SILENCE_WARNINGS").is_ok() {
            return;
        }

        eprintln!(
            "{}: Shuttle runs the `Drop` method of `lazy_static` values at the end of an execution, \
                unlike the actual `lazy_static` implementation. This difference may cause false positives. \
                See https://docs.rs/shuttle/*/shuttle/lazy_static/index.html#warning-about-drop-behavior \
                for details or to disable this warning.",
            "WARNING".yellow(),
        );
    }
}

/// Small wrapper to trigger the warning about drop behavior when a `lazy_static` value drops for
/// the first time.
#[derive(Debug)]
struct DropGuard<T> {
    value: T,
    // We need to stash this config here, because we cannot get it from `ExecutionState` while panicking.
    silence_warnings: bool,
}

impl<T> DropGuard<T> {
    fn new(value: T, silence_warnings: bool) -> Self {
        Self {
            value,
            silence_warnings,
        }
    }
}

impl<T> Drop for DropGuard<T> {
    fn drop(&mut self) {
        maybe_warn_about_drop(self.silence_warnings);
    }
}
