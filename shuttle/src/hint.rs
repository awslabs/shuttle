//! Shuttle's implementation of [`std::hint`].

pub use std::hint::*;

use crate::thread;

/// Emits a machine instruction to signal the processor that it is running in a busy-wait spin-loop
/// (“spin lock”).
pub fn spin_loop() {
    // Probably not necessary, but let's emit it just in case
    std::hint::spin_loop();

    thread::yield_now();
}
