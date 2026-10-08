# Unreleased

* Fix `shuttle-parking_lot`'s `RwLock` refusing a plain read that `parking_lot` grants. While a task held an upgradable read, a queued `write` or `upgradable_read` blocked every new plain read, so a test could report a deadlock that `parking_lot` cannot have (for example, a `read` behind a queued writer while an upgradable read is held), and `try_read` could fail where `parking_lot`'s succeeds. `parking_lot` refuses a plain read only while `WRITER_BIT` is set, and a task that waits behind an upgradable read has not set it. The lock now matches each new request against the free permits of its `BatchSemaphore`, on which a writer reserves the lock (see `BatchSemaphore::acquire_reserving` below) once no writer or upgradable reader holds it, as `parking_lot` sets `WRITER_BIT` only then, and an upgrade reserves it at once. The lock stays as fair as before: every unlock hands the lock to the requests that wait, in order, and outside the states of this bug, Shuttle explores the same schedules as before. The `RwLock` parity tables now match `parking_lot` in all 720 scenarios. Known differences are documented in the crate README: every unlock is fair, while `parking_lot`'s plain unlock is not (#259), `try_write` can succeed on a free lock while `parking_lot`'s `PARKED_BIT` is set, and the `bump` methods unlock and relock also when no task waits. (#376)
* `shuttle-parking_lot`'s `RwLock::is_locked` and `RwLock::is_locked_exclusive` are now reads of the lock state with one scheduling point and no effect, via `BatchSemaphore::load_permits` (see below), like the loads of the lock word that they are in `parking_lot`. They inherited `lock_api`'s defaults, which probe by taking and releasing the lock: the probe transiently held the lock across one or two scheduling points, where a concurrent `try_*` could fail against it — a state `parking_lot` cannot show. `is_locked_exclusive` is true while a writer waits for the readers to leave (`WRITER_BIT`), as before. (#376)
* Add `BatchSemaphore::release_fair`: a release that grants already waiting requests their permits inside the release itself, from the front of the queue and for as long as the released permits last, so that no other request can overtake them. A reserving request at the front that can reserve the semaphore, but not yet take all of its permits, is handed the reservation. While a reservation holds the semaphore the permits are already kept for its holder, and on a strictly fair semaphore every release grants from the front, so in both cases a fair release is a plain `release`. (#376)
* Add `BatchSemaphore::load_permits`: a read of the semaphore's state with one scheduling point and no effect — `None` once the semaphore is closed, otherwise the permits a request could take now. This models reading a lock's atomic state word. (#376)

# 0.9.6 (October 9, 2026)

* Fix a stack overflow when a global `tracing` subscriber at TRACE level calls into Shuttle while it handles an event, for example to call `current::clock()`. Every access to the execution state emits a TRACE event, so the subscriber's own access emitted another event, which called the subscriber again, and so on until the stack overflowed, and the failure report and persisted schedule were lost with the process. Accesses made while that event is being handled no longer emit it. (#382)

* Execution teardown no longer panics or aborts when destructors use Shuttle. When an execution ends, Shuttle drops the tasks that have not finished, so the destructors of everything they own run then. Almost every Shuttle operation panicked in those destructors, because it looked up the running task and there was none; the panic came from a destructor, so the process aborted, and the failure report was lost with it. Among them were `Mutex::lock` and `try_lock` (and the tokio wrapper's), `RwLock`, `BatchSemaphore::try_acquire`, atomics, channel sends, spawning, `thread::current` and `block_on`. (0.9.5 made `current::clock()` safe, but the operations that use the clock still looked up the running task right after it.) (#381)

  Teardown now drops each unfinished task as if the execution had cancelled it as it ended, and while it does, that task is the current task: destructors can use Shuttle as the task could, and what they do is attributed to it. A task that never ran drops its function, and a future task that is parked between polls drops its future, the way an async runtime drops its tasks' futures when it shuts down, each on the task's own stack. So a destructor that blocks, on a lock that another unfinished task holds say, waits until teardown has dropped that task, and one that yields lets the others run. A future task that stopped in the middle of `poll` has its stack unwound once no other task can make progress. Its destructors can use Shuttle too, but cannot wait: one that blocks or panics while the stack unwinds aborts the process, as any panic during unwinding does. A task that catches the unwind is unwound again at its next scheduling point. Teardown runs the tasks in a fixed order, without the scheduler, and doesn't extend the schedule, so it is the same when the schedule is replayed. A destructor that panics fails the test like any other panic, with the schedule persisted. So do destructors that block with nothing left to wake them, which is reported like a deadlock, and a destructor that exceeds the step bound, which counts their scheduling points too. Task-local values, tags and the tasks' spans are dropped as their task, and the execution's statics last, as if by the main thread, once no task is left that could still use them. A lock that a task holds when teardown unwinds its stack is released and not poisoned, and condition variables, barriers and channels forget a task that teardown unwinds while it waits on them. A task that teardown drops is finished, and awaiting its `JoinHandle` returns `JoinError::Cancelled`.

  A parked future is dropped rather than unwound now, so `std::thread::panicking()` is false in its destructors, and checks that are skipped while panicking now run. A tokio-test `Mock` with data left to read, say, now fails the test.

  A failed execution is now torn down too, before its failure is raised; until now, its task-local values and statics were dropped while the failure unwound, when Shuttle was no longer there to call into. It still leaks its tasks' stacks, like a stopped execution, and with them the futures of its parked future tasks, and the functions of its tasks that never ran, unless `UngracefulShutdownConfig::continuation_function_behavior` is `Drop`. Its task-local values and statics are dropped. While a stopped or failed execution is torn down, `ExecutionState::should_stop()` is true, so Shuttle's own destructors skip their bookkeeping, and panics are ignored: a stopped execution doesn't fail, and a failed one reports its original failure.

* Fix a lost panic: a detached task that panicked and switched out while unwinding, to release a lock say, could be dropped at the end of the execution before it finished unwinding. The panic was swallowed, and the thread stayed in a panicking state, which changed how later executions behaved. The execution now waits for a task that is unwinding a panic, even if it is detached, also if it resumed the panic with `std::panic::resume_unwind`, as the tokio wrapper's `watch::Sender::send_modify` does. A stopped execution lets such a task finish unwinding as it is torn down, but no further than catching the panic, so that the panic fails the test. A task whose unwind blocks then, on a lock that a task that no longer runs holds say, fails the test too, and its stack is leaked, which leaves the thread panicking. Tasks share the OS thread, so a task that panics while another task is unwinding a panic can't tell its panic from the other one: if it is still unwinding when the execution ends, that fails the test, and its stack is leaked too. (#381)

* Fix a use after free when a stopped execution's scoped threads never ran: their functions, which borrow from their parent's stack, were dropped after that stack was freed. (#381)

* A panic out of the executor, such as a scheduler's when a replayed schedule doesn't fit the test, no longer skips execution teardown, which aborted the process if a destructor then used Shuttle. (#381)

* `BatchSemaphore`: a task that takes a permit of an unfair semaphore while an `Acquire` of its own is still queued on it is no longer blocked by that `Acquire`. Waking a waker of a task of another execution no longer panics. (#381)

* Add `BatchSemaphore::acquire_reserving`, for unfair semaphores. The request holds nothing and waits like an `acquire` until `min_permits` permits are available, and then reserves the semaphore in the same step: from then on no other request can take a permit, and the request takes its `num_permits` as soon as that many are available. The reservation ends when the request is granted or its future is dropped, and while it lasts, `available_permits` is zero. It models a lock that holds back new requests before it holds everything it asked for, such as a `parking_lot` `RwLock` writer, which sets `WRITER_BIT` and then waits for the readers to leave. `BatchSemaphore::upgrade` on an unfair semaphore now reserves the semaphore too, as soon as no other request holds the reservation, so that nothing can overtake it while it waits for the tasks that hold permits. (#374)

* Publish `shuttle-engine` 0.1.4 and `shuttle-std` 0.1.3, which between them have all of the changes above. `shuttle-std` now requires `shuttle-engine` 0.1.4, whose teardown support it uses. `shuttle-schedulers` is unchanged at 0.1.1; it takes `shuttle-engine` as `^0.1.1`, so it builds against 0.1.4 as it stands. `shuttle`'s own source is unchanged too; it now requires `shuttle-engine` 0.1.4 and `shuttle-std` 0.1.3, so that upgrading to 0.9.6 brings the fixes with it. No wrapper is republished; they get these fixes through `shuttle`.

# 0.9.5 (September 30, 2026)

* Fix a process abort when a `Drop` handler touches a modelled `Mutex`, `RwLock` or semaphore outside a running task. Every `BatchSemaphore` operation looks up the running task's vector clock, and `current::clock()` panicked when there was no running task, when `ExecutionState` was already borrowed, or outside a Shuttle execution. The first case is reached whenever `ExecutionState::cleanup` force-unwinds a task that was still parked when the execution ended, for example after an ordinary test failure. The panic came from a destructor, so the process aborted, and the failure report and persisted schedule were lost with it. `current::clock()` now returns an empty clock in all three cases: an operation that belongs to no task has no causality to record. (#357)

* Fix spans leaking on the thread's entered-span stack while any thread in the process holds a scoped default subscriber, such as one installed with `tracing::subscriber::set_default` by a concurrently running test. Shuttle exited a task's spans by calling `Span::current()` inside `tracing::dispatcher::get_default`, where it returns `Span::none()` while a scoped default exists anywhere in the process, so it exited nothing but still entered the execution's span. Every scheduling step leaked one entry and every `Runner::run` another: a span entered before a context switch was no longer current after it, and once the leaked spans had closed, a later `Span::current()` panicked with `tried to clone a span (Id(..)) that already closed`. Spans are now exited and entered through `Span::with_subscriber`. (#359)

* A task's default `tracing` dispatcher is now saved and restored across context switches, like its span stack. The default dispatcher is per OS thread and every task runs on the same one, so a task that yielded inside `tracing::dispatcher::with_default` left its dispatcher installed while the scheduler and every other task ran: their events went to that dispatcher instead of the test's subscriber, and the task's spans stayed entered, so later events were attributed to the wrong task. If the execution was stopped or panicked while such a task was switched out, the task's default stayed installed after the execution ended too. Shuttle only parks a task's default when it may differ from the execution's, so runs without a scoped default subscriber skip it. (#359)

* Deadlock reports now print blocked tasks' backtraces (captured when `SHUTTLE_CAPTURE_BACKTRACE` is set) with `Display`, in the numbered layout panics use under `RUST_BACKTRACE=1`, instead of with `{:#?}`, which printed one `Debug` record per frame wrapped in `Some(Backtrace [ .. ])`. A task without a captured backtrace prints `<not captured>`. Output without `SHUTTLE_CAPTURE_BACKTRACE` is unchanged. (#360)

* Publish `shuttle-engine` 0.1.3, which has all of the changes above. `shuttle-std` and `shuttle-schedulers` are unchanged at 0.1.2 and 0.1.1; they take `shuttle-engine` as `^0.1.2` and `^0.1.1`, so they build against 0.1.3 as they stand. `shuttle`'s own source is unchanged too; it now requires `shuttle-engine` 0.1.3, so that upgrading to 0.9.5 brings the fixes with it.

# 0.9.4 (September 21, 2026)

* Fix `shuttle-parking_lot`'s upgradable read locks letting a writer in part-way through an upgrade. `RwLockUpgradableReadGuard::upgrade` released the permits it held before taking the rest, so a writer already blocked on the lock was granted it first by the strictly fair semaphore: the value an upgradable reader had just read could change underneath it before its own upgrade completed. Real `parking_lot` guarantees the opposite — it swaps `ONE_READER | UPGRADABLE_BIT` for `WRITER_BIT` in a single atomic step and then waits only for existing readers to drain — and that guarantee is the reason to use an upgradable read at all. The lock is now modelled as permit counts on a single semaphore (shared takes 1, upgradable a strict majority, exclusive all of them), which keeps every transition between the three states atomic. Two further consequences of the old two-semaphore model are fixed along with it: `try_upgrade` no longer fails spuriously when a writer is merely queued, and `downgrade_to_upgradable` no longer deadlocks against a task that is waiting to take an upgradable read. (#351)

* `BatchSemaphore::upgrade` now keeps the permits it already holds and acquires only the missing ones, with priority over queued waiters, instead of releasing its permits and re-acquiring the full count from the back of the queue. An upgrade therefore blocks only on tasks that *currently hold* permits, and cannot be overtaken by a waiter that arrived first. `BatchSemaphore::try_upgrade` is added as the non-blocking counterpart. (#351)

* Fix a process abort when the portfolio runner aborts the remaining executions after finding a counterexample. The drop handlers of the aborted execution then ran in the context of a stopped execution, and any that touched a Shuttle primitive panicked from a drop. A stopped execution now leaks its state on the way out, as a panicking one already did. (#346)

* Performance: `BatchSemaphore` no longer takes a `std::sync::Mutex` in a release-mode assertion on every `Acquire` poll, and allocates its `Waiter` only when an acquire actually blocks. Uncontended synchronization operations (`Mutex`, `RwLock`, `Semaphore`, channels) are 43-50% faster. (#321)

* Performance: `backtrace_enabled` no longer reads the environment on every call. It is called from `Task::block` and `Task::sleep`, so on every block and every `Poll::Pending`, and `std::env::var` takes a lock on the environment and allocates. Lock-heavy workloads are 9-12% faster. (#322)

* Better instrument backtraces for blocked futures. (#215)

* Fix the `annotation` feature. (#334)

* Publish `shuttle-engine`, `shuttle-std` and `shuttle-parking_lot-impl` 0.1.2. `shuttle-schedulers` is unchanged at 0.1.1; it takes `shuttle-engine` as `^0.1.1`, so it builds against 0.1.2 as it stands. `shuttle-parking_lot` itself stays at 0.12.5, mirroring the `parking_lot` version it wraps: it requires the impl as `^0.1.0` and re-exports it with a glob, so it picks the `RwLock` fix up without being republished. The impl now requires `shuttle >=0.9.4`, since the fix is built on the new `BatchSemaphore::upgrade`.

# tokio wrappers (September 6, 2026)

Published `shuttle-tokio-impl-inner` 0.1.2. The already-published `shuttle-tokio-impl` 0.1.1 and `shuttle-tokio` 1.0.0 both require it as `^0.1.1` and re-export it with a glob, so they pick these changes up without being republished. The `shuttle` crate is unchanged at 0.9.3.

* Implement the `mpsc` reservation APIs in `shuttle-tokio`: `Sender::{reserve, try_reserve, reserve_owned, try_reserve_owned}`, `Permit::send` and `OwnedPermit::{send, release, same_channel, same_channel_as_sender}`. `reserve` and `reserve_owned` previously panicked with `unimplemented!()` and the rest were missing. An unused permit returns its capacity to the channel when dropped. Note that `Permit` has gained a lifetime parameter (`Permit<'a, T>`) to match tokio; this is a breaking change in principle, but the only way to obtain a `Permit` used to panic. `reserve_many`/`try_reserve_many` and `PermitIterator` are still unimplemented. (#339)
* Implement `mpsc::Receiver::poll_recv` in `shuttle-tokio`. (#319)

# tokio wrappers (September 4, 2026)

Published `shuttle-tokio` 0.1.1 and 1.0.0, `shuttle-tokio-impl` 0.1.1, `shuttle-tokio-impl-inner` 0.1.1, and `shuttle-tokio-retry` 0.3.0 and `shuttle-tokio-retry-impl` 0.1.0 for the first time. The `shuttle` crate is unchanged at 0.9.3.

* `shuttle-tokio`'s `full` feature now matches tokio's, and tokio's remaining features (including its implicit optional-dependency features) are mirrored as pass-throughs, so switching a crate from `tokio` to `shuttle-tokio` no longer breaks on an unknown feature. (#335, #337)
* `shuttle-tokio` is now versioned 1.0.0, so that it mirrors the version of the crate it wraps like every other wrapper does and a downstream crate can depend on it with the same `version = "1"` requirement it would have used for `tokio`. The 0.1 line is unchanged and still resolves to 0.1.1; moving to the 1.x line is opt-in. (#327)
* Add a `tokio-retry` wrapper, `shuttle-tokio-retry`. (#275)

# 0.9.3 (August 19, 2026)

* Fix `BatchSemaphore` waking the wrong task when an `Acquire` future is polled by a task other than the one that created it (the motivating case is an in-flight acquire cached inside a longer-lived object, such as a tokio `Receiver` that is moved between tasks). Waiters left behind by a cancelled `Acquire` whose task has since finished are now also treated as stale instead of consuming permits or blocking a finished task. (#317)
* Performance: `schedule` no longer iterates over tasks that have already finished. This does not change scheduling decisions. (#318)
* Add READMEs for `shuttle-engine`, `shuttle-schedulers`, `shuttle-std` and the wrapper crates. Make `shuttle-async-stream-impl` publishable. (#315)
* Fix doc comment paths that got broken in the crate refactoring.
* Publish `shuttle-engine`, `shuttle-schedulers` and `shuttle-std` 0.1.1.

# 0.9.2 (August 6, 2026)

* Add support for 128-bit atomics (`AtomicI128`/`AtomicU128`) (#299)
* Implement `Display` for `TaskId` to match tokio's `task::Id` (#307)
* Add support for task abort (#278)
* Refactor the `shuttle` crate into separate internal crates: `shuttle-engine` (core runtime and the `Scheduler` trait), `shuttle-schedulers` (built-in schedulers and `check`/`replay` helpers), and `shuttle-std` (the `std` replacement primitives) (#286, #290, #292, #294). This is an internal reorganization and does not change the `shuttle` public API.
* Publish `shuttle-std`, `shuttle-schedulers` and `shuttle-engine`.

# 0.9.1 (Apr 19, 2026)

* Readd README that was lost in refactoring. (#276)

# 0.9.0 (Apr 19, 2026)

* Fix: `JoinHandle<T>` is now `Send` and `Sync` even if `T` is not.
* Gate vector clocks behind the `vector-clocks` feature flag. (#187)
* Fix: `Once` can now be moved (#188 and #208)
* Various performance improvements (#191, #211)
* `std::sync::{LockResult, PoisonError, TryLockError, TryLockResult}` are now exported from `shuttle::sync` (#198)
* Task names are now logged in the step span (#206)
* Task backtraces are now printed on deadlock if the SHUTTLE_BACKTRACE environment variable is set (#205, #213)
* Spawn events are now traced at `DEBUG` (down from `INFO`) (#211)
* Stable resource ids (#207)
* `UniformRandomWalk` scheduler added (#200)
* Panic path refactored. 1: Aborting panics should more often have their schedule serialized, 2: Schedule is no longer part of the panic message, 3: There will now be multiple schedules serialized on multiple panics, 4: if `Config::immediately_return_on_panic` is set then we will return immediately on a failure and not finish unwinding the panic. (#202)
* Change scheduling points to always precede operations (#216)
* Change the backend for the tasks to be the Corosensei crate instead of the generators crate (#204)
* Add `SHUTTLE_PERSIST_SEED` in the RandomScheduler to persist schedule before running the test (to be used for aborting tests) (#201)
* Add `BatchSemaphore::close_no_scheduling_point` (#227)
* Add config for ungraceful shutdowns (#230)
* Add {RwLock, Mutex}::clear_poison (#233)
* Bump to rand 0.8.6 (#264)

# 0.8.1 (Jun 19, 2025)

* Fix bug in `BatchSemaphore` (#167)
* Fix bug in `RwLock` (#170)
* Add `current::reset_step_count` (#175)
* Add `spawn_local` (#176)
* Add `thread::scope` (#181)
* Add `AbortHandle` (#182)

# 0.8.0 (Sep 30, 2024)

* Add `BatchSemaphore` (#151)
* `block_on` now has one less thread switch point, which breaks schedules. (#155)
* `ReplayScheduler::set_target_clock` added (#156)
* Schedulers now receive references to `Task`s instead of `TaskId`s (#156)
* Expose `check_random_with_seed` (#161)
* Make `check_random` optionally take a seed by providing the environment variable `SHUTTLE_RANDOM_SEED` (#161)
* Shuttle Explorer extension (#163).
* `AnnotationScheduler` and annotated schedule support added under feature "annotation" (#163)

# 0.7.1 (May 31, 2024)

* Implement `try_send` and iterators for `mpsc` channels (#120)
* Implement `get_mut` for `Mutex` and `RwLock` (#120)

# 0.7.0 (March 7, 2024)

* Add support for task labels. These replace task tags, which are deprecated and will be removed in a future release. (#138)
* In the meantime, `Tag`s are now implemented with a trait. This is a breaking change from 0.6.1. (#111)
* Implement `is_finished()` for `future::JoinHandle` (#118)

# 0.6.1 (May 23, 2023)

* Add feature to tag tasks (#98)
* Add scheduler to check for uncontrolled nondeterminism (#96, #97)
* Support spurious wakeups for `thread::park` (#101)
* Support different leaders when `sync::Barrier` is reused (#102)
* Make `{Mutex, Condvar, RwLock}::new` const (#106)
* Improve tracing spans (#99)
* Fix spurious deadlocks with `FuturesUnordered` (#105)
* Split schedule output over multiple lines (#103)
* Bump `futures` dependency (#107)

# 0.6.0 (January 24, 2023)

This version renames the [`silence_atomic_ordering_warning` configuration option](https://docs.rs/shuttle/0.5.0/shuttle/struct.Config.html#structfield.silence_atomic_ordering_warning) to `silence_warnings`, as well as the corresponding environment variables, to enable future warnings to be controlled by the same mechanism.

* Implement `lazy_static` support (#93)

# 0.5.0 (November 22, 2022)

This version updates the embedded `rand` library to v0.8.
Tests that use `shuttle::rand` will need to [update to the v0.8 interface of `rand`](https://github.com/rust-random/rand/blob/master/CHANGELOG.md#080---2020-12-18),
which included some breaking changes.

* Update `rand` and other dependencies (#89)
* Implement abort for `future::JoinHandle` (#87)
* Correctly handle the main thread's thread-local storage destructors (#88)

# 0.4.1 (November 14, 2022)

* Make PCT scheduling not linear in max number of tasks (#84)

# 0.4.0 (September 30, 2022)

* Dependency updates

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
