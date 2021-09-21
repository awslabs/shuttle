# 0.0.7 (September 21, 2021)

* Fix a number of issues in support for async tasks (#50, #51, #52, #54)
* Improve error messages when using Shuttle primitives outside a Shuttle test (#42)
* Add support for thread local storage (the `thread_local!` macro) (#43, #53)
* Add support for `Once` cells (#49)
* Simplify some dependencies to improve build times (#55)
* Move `context_switches` and `my_clock` functions into a new `current` module (#56)

# 0.0.6 (July 8, 2021)

* Add support for `std::sync::atomic` (#33)
* Add `shuttle::context_switches` to get a logical clock for an execution (#37)
* Track causality between threads (#38)
* Better handling for double panics and poisoned locks (#30, #40)
* Add option to not persist failures (#34)

# 0.0.5 (June 11, 2021)

* Fix a performance regression with `tracing` introduced by #24 (#31)
* Include default features for the `rand` crate to fix compilation issues (#29)

# 0.0.4 (June 1, 2021)

* Add a timeout option to run tests for a fixed amount of time (#25)
* Include task ID in all `tracing` log output (#24)
* Implement `thread::current` (#23)

# 0.0.3 (April 13, 2021)

* Update for Rust 1.51 (#11)
* Add option to bound how many steps a test runs on each iterations (#14)
* Remove option to configure the maximum number of threads/tasks (#16, #19)
* Make `yield_now` a hint to the scheduler to allow validating busy loops (#18)
* Add `ReplayScheduler::new_from_file` (#20)

# 0.0.2 (March 19, 2021)

* Add Default impl to RwLock (#7)
* Add option to persist schedules to a file (#4)

# 0.0.1 (March 2, 2021)

* Initial release