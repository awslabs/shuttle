//! The fake clock, and the contexts that `now_v1`, `now_v6` and `now_v7` share.
//!
//! Their state lives in the storage of the running execution, the same storage that Shuttle's own
//! `Once` and `lazy_static` use. Shuttle drops it when the execution ends, so every execution starts
//! from the same state and replays.
//!
//! Each reading of the clock asks Shuttle whether the clock advances, so the time moves by zero or
//! one millisecond between consecutive readings. That is a recorded random choice, like any other
//! value from `shuttle::rand`, so an execution replays. It means a test sees both the case where
//! two UUIDs share a millisecond, which is what exercises a `ContextV7`'s counter, and the case
//! where the second one is in the next millisecond. Where the scheduler provides no random data, as
//! under `shuttle::check_dfs`, each reading moves the clock on by exactly one millisecond instead,
//! which adds nothing to the schedule. Reading the clock is not a scheduling point, as reading the
//! system clock isn't.

// `shuttle` 0.9.2 and later run on `shuttle-engine`, so the engine whose storage this module uses
// is the one running the test, and its `rand` is the generator Shuttle records.
use shuttle::rand::{Rng, try_thread_rng};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::storage::StorageKey;
use std::cell::Cell;
#[cfg(feature = "v7")]
use std::cell::RefCell;

/// Where the fake clock starts in every execution: 2026-01-01T00:00:00Z, in seconds since the Unix
/// epoch.
const EPOCH_SECONDS: u64 = 1_767_225_600;

/// The state that lasts for one execution.
struct PerExecution {
    /// How many milliseconds after `EPOCH_SECONDS` the next reading of the clock is.
    next_millis: Cell<u64>,
    /// The state of the context that `now_v1` and `now_v6` share, once seeded.
    #[cfg(all(any(feature = "v1", feature = "v6"), feature = "rng"))]
    context_v1: Cell<Option<u16>>,
    /// The context that `now_v7` shares.
    #[cfg(feature = "v7")]
    context_v7: RefCell<crate::timestamp::context::ContextV7>,
}

impl PerExecution {
    fn new() -> Self {
        PerExecution {
            next_millis: Cell::new(0),
            #[cfg(all(any(feature = "v1", feature = "v6"), feature = "rng"))]
            context_v1: Cell::new(None),
            #[cfg(feature = "v7")]
            context_v7: RefCell::new(crate::timestamp::context::ContextV7::new()),
        }
    }
}

fn key() -> StorageKey {
    static ID: u8 = 0;
    // The second half tells this slot apart from Shuttle's own (`Once` uses 0x2, `lazy_static`
    // 0x3), whose first half is also an address.
    StorageKey(&ID as *const u8 as usize, 0x7575_6964)
}

/// Runs `f` on this execution's state. `f` must not call into Shuttle, which would find the
/// execution's state already borrowed.
fn with_state<T>(f: impl FnOnce(&PerExecution) -> T) -> T {
    ExecutionState::with(|state| {
        if state.get_storage::<_, PerExecution>(key()).is_none() {
            state.init_storage(key(), PerExecution::new());
        }
        f(state.get_storage(key()).expect("the state was just initialized"))
    })
}

/// How far a reading moves the fake clock on, in milliseconds: zero or one, as Shuttle chooses, or
/// exactly one where the scheduler provides no random data.
fn step() -> u64 {
    match try_thread_rng() {
        Some(mut rng) => u64::from(rng.gen_bool(0.5)),
        None => 1,
    }
}

/// Reads the fake clock, as seconds and nanoseconds since the Unix epoch. The first reading of an
/// execution is the epoch itself, and each reading moves the clock on by [`step`].
pub(crate) fn now() -> (u64, u32) {
    // Ask before taking the state, as a Shuttle primitive can't run while the execution's state is
    // borrowed.
    let advance = step();
    let millis = with_state(|state| {
        let millis = state.next_millis.get();
        state.next_millis.set(millis + advance);
        millis
    });
    (EPOCH_SECONDS + millis / 1000, (millis % 1000) as u32 * 1_000_000)
}

/// The timestamp for `now_v1` and `now_v6`: the fake clock, with a counter from the shared
/// context, which is seeded with random bits from Shuttle (see `crate::rng`).
#[cfg(all(any(feature = "v1", feature = "v6"), feature = "rng"))]
pub(crate) fn now_v1() -> crate::Timestamp {
    let (seconds, subsec_nanos) = now();
    let count = match with_state(|state| state.context_v1.get()) {
        Some(count) => count,
        None => crate::rng::u16(),
    };
    let context = crate::timestamp::context::ContextV1::new(count);
    let ts = crate::Timestamp::from_unix(&context, seconds, subsec_nanos);
    with_state(|state| state.context_v1.set(Some(context.count())));
    ts
}

/// The timestamp for `now_v7`: the fake clock, with a counter from the shared context.
#[cfg(feature = "v7")]
pub(crate) fn now_v7() -> crate::Timestamp {
    let (seconds, subsec_nanos) = now();
    // Generate from a copy, as reseeding takes a random value from Shuttle, and store it back. No
    // other task runs in between, as there is no scheduling point.
    let context = with_state(|state| state.context_v7.borrow().copy());
    let ts = crate::Timestamp::from_unix(&context, seconds, subsec_nanos);
    with_state(|state| *state.context_v7.borrow_mut() = context);
    ts
}
