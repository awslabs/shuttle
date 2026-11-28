//! The clock that the running execution reads.
//!
//! Its state lives in the storage of the running execution, which Shuttle drops when the execution
//! ends, so every execution starts from the same state and replays. Reading it is not a scheduling
//! point, as reading the system clock is not one either.

use shuttle::rand::{Rng, try_thread_rng};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::storage::StorageKey;
use std::cell::Cell;
use std::time::Duration;

/// Where the clock starts in every execution: 2026-01-01T00:00:00Z, as a duration since the Unix
/// epoch. [`crate::time::SystemTime::now`] returns the Unix epoch plus the reading, and
/// [`crate::time::Instant::now`] the same reading after its own base, so the two agree.
pub(crate) const START: Duration = Duration::from_secs(1_767_225_600);

/// The state that lasts for one execution.
struct Clock {
    /// How many nanoseconds after [`START`] the next reading is.
    nanos: Cell<u64>,
}

fn key() -> StorageKey {
    static ID: u8 = 0;
    // The second half tells this slot apart from Shuttle's own (`Once` uses 0x2, `lazy_static`
    // 0x3), whose first half is also an address.
    StorageKey(&ID as *const u8 as usize, 0x7469_6d65)
}

/// Returns the clock's state in the running execution, creating it if this is the first time the
/// execution looks at the clock.
fn with_clock<R>(f: impl FnOnce(&Clock) -> R) -> R {
    ExecutionState::with(|state| {
        if state.get_storage::<_, Clock>(key()).is_none() {
            state.init_storage(key(), Clock { nanos: Cell::new(0) });
        }
        f(state.get_storage(key()).expect("the clock was just initialized"))
    })
}

/// Reads the clock without advancing it and without a draw from Shuttle, so that a timer can check
/// its deadline without changing the readings the program sees. Panics outside of a Shuttle test.
pub(crate) fn peek() -> Duration {
    with_clock(|clock| START + Duration::from_nanos(clock.nanos.get()))
}

/// Moves the clock forward to `reading`, if it is behind it, so that the reading after a timer has
/// fired is no earlier than the timer's deadline. Panics outside of a Shuttle test.
pub(crate) fn advance_to(reading: Duration) {
    let target = reading.saturating_sub(START).as_nanos();
    let target = u64::try_from(target).unwrap_or(u64::MAX);
    with_clock(|clock| clock.nanos.set(clock.nanos.get().max(target)));
}

/// How far a reading moves the clock on: zero or one nanosecond, as a random bool from Shuttle
/// decides, or exactly one nanosecond if the scheduler provides no random data, as under
/// `shuttle::check_dfs`.
///
/// Shuttle records the bool in the schedule, so a schedule that depends on whether two readings
/// differ replays, and `check_uncontrolled_nondeterminism` does not report the clock. The fixed
/// step adds nothing to the schedule, so reading the clock adds no choice to an exhaustive search,
/// and a replay of such a schedule takes the fixed step too.
fn step() -> u64 {
    match try_thread_rng() {
        Some(mut rng) => u64::from(rng.gen_bool(0.5)),
        None => 1,
    }
}

/// Reads the clock, and moves it on by [`step`]. Panics outside of a Shuttle test, as the rest of
/// Shuttle does.
pub(crate) fn read() -> Duration {
    // Ask before taking the execution's state below: asking takes it too, and Shuttle panics on the
    // second borrow.
    let advance = step();
    with_clock(|clock| {
        let nanos = clock.nanos.get();
        clock.nanos.set(nanos.saturating_add(advance));
        START + Duration::from_nanos(nanos)
    })
}
