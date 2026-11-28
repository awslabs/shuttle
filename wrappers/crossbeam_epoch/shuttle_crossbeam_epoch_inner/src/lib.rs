//! This crate contains Shuttle's internal implementation of the [`crossbeam-epoch`] crate.
//! Do not depend on this crate directly. Use the [`shuttle-crossbeam-epoch`] crate, which
//! conditionally exposes this implementation with the `shuttle` feature or the original crate
//! without it.
//!
//! [`crossbeam-epoch`]: <https://crates.io/crates/crossbeam-epoch>
//! [`shuttle-crossbeam-epoch`]: <https://crates.io/crates/shuttle-crossbeam-epoch>
//!
//! # The model
//!
//! [`Atomic`], [`Owned`], [`Shared`], [`Guard`], [`Collector`], [`LocalHandle`], [`pin`],
//! [`is_pinned`], [`default_collector`] and [`unprotected`] have crossbeam-epoch 0.9.21's API.
//! [`Pointable`], [`Pointer`] and [`CompareAndSetOrdering`] are crossbeam-epoch's own traits, so an
//! `impl Pointable for MyType` works in both modes.
//!
//! The pointers are crossbeam's: an [`Atomic`] holds one word, the address of an object from
//! [`Pointable::init`] with a tag in the bits the alignment leaves unused. The two changes are
//! where that word lives, and that every operation on it goes through Shuttle:
//!
//! | Operation | Scheduling points |
//! |---|---|
//! | `Atomic`: `load`, `load_consume`, `store`, `swap`, `compare_exchange`, `compare_exchange_weak`, `compare_and_set`, `compare_and_set_weak`, `fetch_and`, `fetch_or`, `fetch_xor`, `Debug`, `Pointer`, `Clone` | 1 |
//! | `Atomic::fetch_update` | 1 per load and per attempt |
//! | `pin`, `LocalHandle::pin`, and dropping a `Guard`, when the participant was not pinned already | 1 |
//! | `Guard::flush`, `Guard::repin` | 1 |
//! | `Guard::repin_after` | 2, one before `f` and one after |
//! | `Atomic::{new, init, null, from, into_owned, try_into_owned}`, every `Owned` and `Shared` operation, `is_pinned`, `Guard::defer*`, `Collector::{new, register, clone}`, `LocalHandle::{is_pinned, collector}`, `unprotected`, reentrant pinning, dropping a `Guard` that is not the last one | 0 |
//!
//! So Shuttle explores every order in which concurrent operations on an `Atomic` take effect, and a
//! loop that waits by pinning or by loading lets other tasks run. Each `Atomic` operation also makes
//! the calling task and the word exchange vector clocks, as an operation on one of Shuttle's atomics
//! does. Shuttle models every memory ordering as `SeqCst` and warns the first time it is given
//! another one, so bugs that need a relaxed ordering to show up are not found.
//!
//! # Nothing is reclaimed: the leak model
//!
//! **A deferred function is never called.** [`Guard::defer`], [`Guard::defer_unchecked`] and
//! [`Guard::defer_destroy`] store it for the running execution, and the end of the execution drops
//! it without calling it. So:
//!
//! - An object whose destruction a consumer defers stays allocated for the rest of the process: a
//!   test leaks one object per `defer_destroy`, times the number of executions. A Shuttle test
//!   makes thousands of executions, so a data structure that allocates heavily per execution may
//!   want a smaller iteration count under this crate.
//! - What a deferred closure *captured* is dropped when the execution ends, since the closure is.
//!   A closure that owns an [`Owned`] therefore frees it then; the usual `move || ptr.into_owned()`
//!   of a [`Shared`] captures a `Copy` word and frees nothing.
//! - **There is no use-after-free.** Every pointer a consumer can load points to a live object,
//!   whichever schedule runs, and whether or not any guard is pinned. A test that fails under this
//!   crate fails on its own logic, not on reclamation. The flip side is that a bug where an object
//!   is reclaimed too early (a missing `pin`, a `Shared` kept past its guard) cannot be caught here.
//! - [`Guard::flush`] and [`Guard::repin`] take their scheduling points but collect nothing, and
//!   dropping a [`Collector`] runs nothing, where crossbeam runs what its participants deferred.
//!
//! An [`unprotected`] guard is the exception, as in crossbeam: a function deferred on it runs
//! immediately, and `defer_destroy` on it frees the object right away. That is the call a consumer
//! makes when it knows no one else can reach the object, which is what `unprotected`'s contract asks
//! of it.
//!
//! Pinning is still modelled: it is reentrant, [`is_pinned`] answers for the running task, and
//! pinning and unpinning take the scheduling points crossbeam's do.
//!
//! # State per execution
//!
//! An [`Atomic`]'s word lives in the storage of the running execution, and starts every execution at
//! the value the pointer was constructed with. So a `static Atomic`, or one the test closure
//! captured, is as constructed at the start of every execution, which keeps an execution from
//! depending on the ones before it. Together with the leak model, that word always points at a live
//! object: nothing a later execution can load was freed by an earlier one.
//!
//! Each participant's pinning and each collector's deferred functions are per execution too, and the
//! participant of the default collector is per Shuttle task, in that task's task-local storage. The
//! collectors' *identities* last for the whole process, as crossbeam's do, so that
//! [`default_collector`] can hand out a `&'static Collector` and two `Collector`s compare equal when
//! they are clones of each other.
//!
//! # Differences from crossbeam-epoch
//!
//! Besides the leak model and the per-execution state:
//!
//! - An [`Atomic`] operation must run inside a Shuttle execution, and panics outside of one with
//!   Shuttle's "Are you accessing a Shuttle primitive outside of a Shuttle test?", as do [`pin`] and
//!   [`is_pinned`]. [`Owned`], [`Shared`], [`unprotected`], `Collector::new`, `register`,
//!   constructing an `Atomic` and dropping one work anywhere.
//! - `compare_exchange_weak` and `compare_and_set_weak` do not fail spuriously, since Shuttle's
//!   atomics do not; crossbeam's may on hardware with load-linked/store-conditional instructions.
//!   A consumer's retry loop is still correct, and still explored, because a real compare-and-swap
//!   can fail for the other reason: another task got in first.
//! - `load_consume` is `load(Acquire, ..)`, since Shuttle treats every ordering as `SeqCst`.
//! - `Atomic::clone` gives the copy the word it reads as the copy's construction value, so the copy
//!   starts every later execution there.
//! - The free `defer*` of a non-`'static` closure is held until the execution ends, where
//!   crossbeam's may run it sooner; its safety contract is unchanged, and this crate's version is
//!   the weaker requirement.
//! - The pointers do not take part in Shuttle's annotated schedules as named resources, and the
//!   model has no epochs, no global queue and no `Collector`-level bookkeeping a consumer can see.
//! - A `static Atomic` whose object is freed through an [`unprotected`] guard, without the `Atomic`
//!   being dropped, has a dangling word in the next execution, because the word resets. Taking the
//!   object out with [`Atomic::into_owned`], which consumes the pointer, does not have that problem.
//!
//! # Example
//!
//! ```
//! use shuttle_crossbeam_epoch_inner::{self as epoch, Atomic, Owned};
//! use std::sync::atomic::Ordering::SeqCst;
//!
//! shuttle::check_dfs(
//!     || {
//!         let a = Atomic::new(0u64);
//!         let writer = shuttle::thread::spawn(move || {
//!             let guard = &epoch::pin();
//!             let old = a.swap(Owned::new(1), SeqCst, guard);
//!             // The model never frees it, so this leaks instead of being a use-after-free:
//!             unsafe { guard.defer_destroy(old) };
//!         });
//!         writer.join().unwrap();
//!     },
//!     None,
//! );
//! ```

// The scheduling points and the storage come from `shuttle-engine`. `shuttle` provides the atomics
// the pointers are built on, the task-local storage of the default collector's participants, and the
// engine that a test runs with (0.9.2 or later, which is built on `shuttle-engine`).
mod atomic;
mod collector;
mod default;
mod guard;
mod per_execution;

pub use crate::atomic::{Atomic, CompareExchangeError, Owned, Pointable, Pointer, Shared};
#[allow(deprecated)]
pub use crate::atomic::{CompareAndSetError, CompareAndSetOrdering};
pub use crate::collector::{Collector, LocalHandle};
pub use crate::default::{default_collector, is_pinned, pin};
pub use crate::guard::{Guard, unprotected};

/// The second half of this crate's keys in the execution storage, which tells them apart from the
/// keys of Shuttle's own primitives and of the other wrappers.
const STORAGE_TAG: usize = 0x6362_6570; // "cbep"
