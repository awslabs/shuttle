# 0.4.0 (September 30, 2022)

* Depdendency updates

# 0.3.0 (August 29, 2022)

Note that clients using async primitives provided by Shuttle (task `spawn`, `block_on`, `yield_now`) will
need to be updated due to the renaming of the `asynch` module to `future` in this release.

* Rust 2021 conversion and dependency bumps (#76)
* Implement `thread::park` and `thread::unpark` (#77)
* Implement `std::hint` (#78)
* Rename the `asynch` module to `future` (#79)

# 0.2.0 (July 7, 2022)

Note that failing test schedules created by versions of Shuttle before 0.2.0 will not successfully
`replay` on version 0.2.0, and vice versa, as the changes below affect `Mutex` and `RwLock`
scheduling decisions.

* Implement `Mutex::try_lock` (#71)
* Implement `RwLock::{try_read, try_write}` (#72)
* Export a version of `std::sync::Weak` (#69)
* Provide better error messages for deadlocks caused by non-reentrant locking (#66)

# 0.1.0 (April 5, 2022)

* Implement `Condvar::wait_while` and `Condvar::wait_timeout_while` (#59)
* Remove implicit `Sized` bounds on `Mutex` and `RwLock` (#62)
* Dependency updates (#58, #60)

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
