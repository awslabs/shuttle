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