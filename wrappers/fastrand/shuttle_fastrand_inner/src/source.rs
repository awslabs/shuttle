//! Where this crate's randomness comes from: Shuttle's random number generator, which records every
//! value it hands out in the schedule. Drawing is not a scheduling point, and outside of a Shuttle test
//! it panics, as the rest of Shuttle does.

// The trait of rand_core 0.6, which `shuttle::rand`'s generator implements.
use shuttle::rand::RngCore as _;

pub(crate) fn next_u64() -> u64 {
    shuttle::rand::thread_rng().next_u64()
}
