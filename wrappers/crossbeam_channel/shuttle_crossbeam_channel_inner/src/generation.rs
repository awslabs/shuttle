//! Which execution a channel's state belongs to.
//!
//! A channel keeps its state in itself, since its messages need not be `'static`, which the
//! execution's storage requires. A channel that outlives the execution that created it (one kept
//! in a `static` that Shuttle does not reset, say) would carry messages and blocked tasks into the
//! next execution, where the tasks do not exist. So the state remembers the execution it belongs
//! to, and the first operation in another execution starts it over.

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
    // `OnceLock` 0x4) and from `shuttle-time`'s clock, whose first halves are also addresses.
    StorageKey(&ID as *const u8 as usize, 0x6362_6368)
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
