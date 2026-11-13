//! The slot a [`task_local!`](crate::task_local) key uses whenever Shuttle has no current task.
//!
//! tokio keeps a key's values in a `std::thread_local!` that the `task_local!` macro declares in
//! the caller. This crate keeps them in the current Shuttle task's own slot instead (see
//! [`LocalKey`](crate::task::LocalKey)), and falls back to a thread-local one whenever there is no
//! current task: outside of a Shuttle test, and while Shuttle updates its own state.
//!
//! That fallback is a [`FallbackKey`] rather than a `std::thread_local!` of its own, for two
//! reasons.
//!
//! * A `std::thread_local!` in the macro's expansion is a `std::thread_local!` *call in the crate
//!   that invokes `task_local!`*. Lints fire on macro calls inside other crates' expansions, with
//!   the span in the defining crate's source, so a `#[deny]`/`#[forbid]` of clippy's
//!   `disallowed_macros` for `std::thread_local` in a consumer turned every `task_local!` in that
//!   consumer into an error. The expansion now names only types of this crate, whose own
//!   `std::thread_local!` is compiled here, where the consumer's lint levels do not reach.
//! * `thread_local!` takes no type parameters, so there is no way to declare one slot per key
//!   from inside this crate. One keyed registry, declared here once, covers every key.

use std::any::Any;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

std::thread_local! {
    /// The slots of the keys this OS thread has used the fallback of, by the address of the key's
    /// static. Entries are only ever added, so a slot lives as long as the thread, like tokio's
    /// thread-local.
    ///
    /// The slots are `Rc`s rather than `Box`es so that a slot can be handed out without holding a
    /// borrow of the registry: a caller keeps the slot alive for as long as it uses it, and code
    /// that runs in the meantime can use other keys, and create their slots.
    static SLOTS: RefCell<Vec<(usize, Rc<dyn Any>)>> = const { RefCell::new(Vec::new()) };
}

/// Makes a slot for a key of type `T`.
fn new_slot<T: 'static>() -> Rc<dyn Any> {
    Rc::new(RefCell::new(Option::<T>::None))
}

/// A key into the thread-local slots that [`task_local!`](crate::task_local) falls back to. One
/// slot per key per OS thread, created when the thread first uses the key.
///
/// This is an implementation detail of [`task_local!`](crate::task_local); the macro declares one
/// of these for each key it is given.
#[doc(hidden)]
pub struct FallbackKey<T: 'static> {
    /// Makes this key's slot. Also keeps the type from being zero-sized, which `PhantomData`
    /// alone would leave it: each key is identified by the address of its static, and two
    /// zero-sized statics may share one.
    new_slot: fn() -> Rc<dyn Any>,
    /// A key holds no `T` of its own, only the means to reach each thread's, so like
    /// `std::thread::LocalKey` it is `Send`, `Sync` and unwind safe whatever `T` is.
    /// `fn() -> T` gives it all four.
    _p: PhantomData<fn() -> T>,
}

impl<T: 'static> FallbackKey<T> {
    /// A key whose slots start out empty.
    pub const fn new() -> Self {
        Self {
            new_slot: new_slot::<T>,
            _p: PhantomData,
        }
    }

    /// Runs `f` on this thread's slot of this key, creating the slot if the thread has not used
    /// the key before.
    ///
    /// Returns `None` once this thread's slots have been destroyed, which happens while the thread
    /// exits, just as `std::thread::LocalKey::try_with` returns an error then.
    pub fn try_with<F, R>(&'static self, f: F) -> Option<R>
    where
        F: FnOnce(&RefCell<Option<T>>) -> R,
    {
        // As shuttle-engine identifies the keys of its own thread-locals.
        let id = self as *const Self as usize;
        let slot = SLOTS
            .try_with(|slots| {
                let mut slots = slots.borrow_mut();
                match slots.iter().find(|(key, _)| *key == id) {
                    Some((_, slot)) => Rc::clone(slot),
                    None => {
                        // A linear scan: a program has few task-local keys, and this is only
                        // reached when Shuttle has no current task.
                        let slot = (self.new_slot)();
                        slots.push((id, Rc::clone(&slot)));
                        slot
                    }
                }
            })
            .ok()?;

        // The registry is no longer borrowed, and the `Rc` keeps the slot alive for the call, so
        // `f` may use this key again, and other keys, and create their slots.
        Some(f(slot.downcast_ref().expect("a key's slot holds the key's own type")))
    }
}

impl<T: 'static> Default for FallbackKey<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: 'static> std::fmt::Debug for FallbackKey<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FallbackKey").finish_non_exhaustive()
    }
}
