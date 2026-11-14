//! The default garbage collector.
//!
//! Its identity lasts for the whole process, as crossbeam's does, so that [`default_collector`] can
//! hand out a `&'static Collector`. Everything it holds belongs to the running execution: the
//! functions it is handed, and each participant's pinning. A participant is registered for each
//! Shuttle task that pins, in that task's Shuttle-task-local storage, so a task gets a fresh one in
//! every execution, as crossbeam's thread does on a fresh thread.

use crate::collector::{Collector, LocalHandle};
use crate::guard::Guard;
use std::sync::OnceLock;

fn collector() -> &'static Collector {
    /// The global data for the default garbage collector.
    static COLLECTOR: OnceLock<Collector> = OnceLock::new();
    COLLECTOR.get_or_init(Collector::new)
}

shuttle::thread_local! {
    /// The participant of the running task in the default garbage collector.
    static HANDLE: LocalHandle = collector().register();
}

/// Pins the current thread.
///
/// One scheduling point, unless the task is pinned already. Panics outside of a Shuttle execution.
#[inline]
pub fn pin() -> Guard {
    with_handle(|handle| handle.pin())
}

/// Returns `true` if the current thread is pinned.
///
/// Not a scheduling point. Panics outside of a Shuttle execution.
#[inline]
pub fn is_pinned() -> bool {
    with_handle(|handle| handle.is_pinned())
}

/// Returns the default global collector.
pub fn default_collector() -> &'static Collector {
    collector()
}

#[inline]
fn with_handle<F, R>(mut f: F) -> R
where
    F: FnMut(&LocalHandle) -> R,
{
    HANDLE.try_with(|h| f(h)).unwrap_or_else(|_| f(&collector().register()))
}
