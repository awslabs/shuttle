//! The `std::hint` spin hint, invisible to Shuttle.

/// [`std::hint::spin_loop`]: the bare CPU hint, with no scheduling point.
///
/// Shuttle's `spin_loop` yields to the scheduler, so a spin loop lets the task it waits for run.
/// This one doesn't: inside a Shuttle test a loop that spins on another task's work never lets
/// that task run, and the test hangs. Use it only for a wait that no Shuttle
/// task has to finish, such as one on a real OS thread or on hardware.
#[inline]
pub fn spin_loop() {
    std::hint::spin_loop()
}
