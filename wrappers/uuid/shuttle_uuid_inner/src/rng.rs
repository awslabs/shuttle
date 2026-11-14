//! Randomness, from one of two sources:
//!
//! - Shuttle's random number generator, where the scheduler provides random data, as the random
//!   and PCT schedulers do. Shuttle records each value in the schedule.
//! - Where the scheduler provides no random data, as under `shuttle::check_dfs`, a generator of
//!   the running execution's own: a counter of the values drawn from it so far, which starts at
//!   zero in every execution, mixed into the value by SplitMix64's output function. It adds nothing
//!   to the schedule, so an exhaustive search explores the same schedules as without it, and the
//!   values an execution gets depend only on the order in which it draws them, which the schedule
//!   decides. The same schedule therefore gives the same UUIDs, whenever it runs, and no two of the
//!   generator's values in an execution are equal.

// Which of these are used depends on the features, as in uuid.
#![allow(dead_code)]

use shuttle::rand::{RngCore, try_thread_rng};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::storage::StorageKey;
use std::cell::Cell;

pub(crate) fn bytes() -> [u8; 16] {
    let mut bytes = [0; 16];
    match try_thread_rng() {
        Some(mut rng) => rng.fill_bytes(&mut bytes),
        // A value for each half, in the byte order Shuttle's generator fills them in.
        None => {
            let first = counter_u64();
            let second = counter_u64();
            bytes[..8].copy_from_slice(&first.to_le_bytes());
            bytes[8..].copy_from_slice(&second.to_le_bytes());
        }
    }
    bytes
}

pub(crate) fn u128() -> u128 {
    u128::from_ne_bytes(bytes())
}

pub(crate) fn u64() -> u64 {
    match try_thread_rng() {
        Some(mut rng) => rng.next_u64(),
        None => counter_u64(),
    }
}

pub(crate) fn u16() -> u16 {
    u64() as u16
}

/// The state of the execution's own generator.
struct Counter {
    /// How many values the execution has drawn from it.
    drawn: Cell<u64>,
}

fn key() -> StorageKey {
    static ID: u8 = 0;
    // The second half tells this slot apart from Shuttle's own (`Once` uses 0x2, `lazy_static`
    // 0x3), whose first half is also an address, and from the fake clock's (0x7575_6964).
    StorageKey(&ID as *const u8 as usize, 0x7575_726e)
}

/// The next value of the execution's own generator. Takes the execution's state, so it must not be
/// called while that is borrowed.
fn counter_u64() -> u64 {
    let drawn = ExecutionState::with(|state| {
        if state.get_storage::<_, Counter>(key()).is_none() {
            state.init_storage(key(), Counter { drawn: Cell::new(0) });
        }
        let counter: &Counter = state.get_storage(key()).expect("the counter was just initialized");
        let drawn = counter.drawn.get();
        counter.drawn.set(drawn.wrapping_add(1));
        drawn
    });
    split_mix_64(drawn)
}

/// The value that SplitMix64 seeded with zero returns after `drawn` others. Its state advances by an
/// odd constant, and its output function is a bijection, so the first 2^64 values all differ.
fn split_mix_64(drawn: u64) -> u64 {
    let mut z = drawn.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    #[test]
    fn split_mix_64_is_split_mix_64() {
        // The first values of SplitMix64 seeded with zero, from its reference implementation.
        let first = (0..3).map(super::split_mix_64).collect::<Vec<_>>();
        assert_eq!(
            first,
            [0xE220_A839_7B1D_CDAF, 0x6E78_9E6A_A1B9_65F4, 0x06C4_5D18_8009_454F]
        );
    }
}
