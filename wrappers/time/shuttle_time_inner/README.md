# Shuttle support for `std::time`

This crate contains the implementation that enables testing of applications that read `std::time`'s
clocks with Shuttle. It should not be depended on directly, depend on `shuttle-time` instead.

`Instant::now()` and `SystemTime::now()` read a counter of nanoseconds that belongs to the running
execution, which starts over in every execution, so that a failing schedule replays. Each reading
advances it by zero or one nanosecond, as Shuttle chooses, or by exactly one where the scheduler
provides no random data, as under `shuttle::check_dfs`. **The timestamps track nothing real.** This
is not virtual time: the crate models no timers. See the crate documentation for the clock, and for
the limitations.
