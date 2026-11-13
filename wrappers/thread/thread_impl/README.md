# Shuttle support for `std::thread`

This crate contains the implementation that enables testing of applications that use `std::thread`,
`thread_local!` and `std::hint::spin_loop` with Shuttle. It should not be depended on directly,
depend on `shuttle-thread` instead.
