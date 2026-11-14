//! Which execution a primitive's state belongs to.
//!
//! A primitive keeps its state in itself, since the state need not be `'static`, which the
//! execution's storage requires. A primitive that outlives the execution that created it (one kept
//! in a `static`, say) would carry state into the next execution, where the clocks name tasks that
//! do not exist and a parked task is not the one its identifier now means. So the state remembers
//! the execution it belongs to, and the first operation in another execution starts it over.
//!
//! `shuttle-crossbeam-channel` has its own copy of this, under its own key. The two become one
//! `PerExecution` once the branch that gives Shuttle's own primitives per-execution state lands.

use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::storage::StorageKey;
use std::sync::atomic::{AtomicU64, Ordering};

/// The next generation to hand out. Zero is never handed out.
static NEXT: AtomicU64 = AtomicU64::new(1);

/// The running execution's generation, in its storage.
struct Generation(u64);

fn key() -> StorageKey {
    static ID: u8 = 0;
    // The second half tells this slot apart from Shuttle's own (`Once` uses 0x2, `lazy_static` 0x3,
    // `OnceLock` 0x4), from `shuttle-time`'s clock and from the other crossbeam wrappers', whose
    // first halves are also addresses.
    StorageKey(&ID as *const u8 as usize, 0x6362_7574)
}

/// The running execution's generation: a number that no other execution in this process has.
/// Panics outside of a Shuttle execution.
pub(crate) fn current() -> u64 {
    ExecutionState::with(|state| {
        if let Some(generation) = state.get_storage::<_, Generation>(key()) {
            return generation.0;
        }
        let generation = NEXT.fetch_add(1, Ordering::Relaxed);
        state.init_storage(key(), Generation(generation));
        generation
    })
}
