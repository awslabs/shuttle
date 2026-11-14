# Shuttle support for `parking_lot`

This crate contains the implementation that enables testing of applications that use
[parking_lot](https://crates.io/crates/parking_lot) with Shuttle. It should not be depended on
directly, depend on `shuttle-parking_lot` instead.

## Implementation

Like the real `parking_lot` crate, the locks are layered on top of the
[`lock_api`](https://crates.io/crates/lock_api) crate. This crate supplies Shuttle-backed *raw*
locks (`RawMutex`, `RawFairMutex` and `RawRwLock`, built on Shuttle's `BatchSemaphore`) that
implement the relevant `lock_api` raw traits; the user-facing `Mutex`/`FairMutex`/`ReentrantMutex`/
`RwLock`, their guards, the mapped guards, and the `Arc`-based guards are the generic `lock_api`
types specialised to those raw locks. As a result the guard surface — including
`lock_arc`/`read_arc`/`write_arc`, upgradable reads, and downgrading — matches `parking_lot`, and
the `lock_api` raw types are re-exported so code that names `parking_lot::RawMutex`,
`parking_lot::RawRwLock`, `parking_lot::ArcRwLockReadGuard`, etc. continues to compile under the
Shuttle feature. `Condvar` and `Once`, which `parking_lot` doesn't build on `lock_api` either, are
Shuttle models of their own.

Every primitive can be `const`-constructed and live in a `static`, where it outlives a Shuttle
execution and is shared by the Shuttle tests that run at the same time on other threads. So a
primitive's Shuttle state — a lock's semaphore, a condvar's waiters, a `Once`'s progress — belongs
to the running execution, and every execution starts with the primitive unlocked, unwaited and
uncompleted. The data a lock protects is the caller's, kept inside the lock by `lock_api`, so the
data of a `static` lock carries over from one execution to the next; a lock that a test needs to
start afresh in every execution belongs in a `shuttle::lazy_static!`. For the same reason, two
executions can't hold a lock's data at once: one whose own state grants it the lock while a guard
of another execution holds the data panics rather than hand out a second reference to it, and says
whose guard it is. Either a test that runs at the same time on another thread shares the `static`
(such tests have to take turns, or run one at a time), or an execution that has ended left the
guard behind (a leaked or forgotten one), which holds the data for good.

## Limitations

Shuttle has no clock to measure a timeout against, so the timed operations never time out: the
`*_for` and `*_until` lock methods wait until they get the lock and return `true`, and a timed
`Condvar` wait waits until it is notified. A task that would time out in a real run waits instead,
and if nothing ever releases or notifies it, Shuttle reports a deadlock.

A `static` `Once` runs a closure in every execution, where a real one runs one closure per process.
`shuttle::sync::Once` and `shuttle::lazy_static!` behave the same way.

Unlocking is strictly fair: the lock goes to the task that has waited longest. Real `parking_lot`
lets a running thread take the lock ahead of a woken waiter unless the unlock is `unlock_fair` or
`bump`, and its `RwLock` wakes a filtered group of waiters. So a `Mutex` behaves here as a
`FairMutex` does, and schedules that only an unfair hand-off can reach are not explored. See
[#259](https://github.com/awslabs/shuttle/issues/259).

Not provided: `deadlock_detection`'s `parking_lot::deadlock` module (its feature is accepted and has
no effect, since Shuttle detects deadlocks itself), and `Condvar`'s and `Once`'s `Arc`-free raw
APIs, which `parking_lot` doesn't have either. As in `parking_lot`, guards are `!Send` by default
and become `Send` (when the data allows) under the `send_guard` feature. If your project needs
functionality which is not currently supported, please file an issue or, better yet, open a PR to
contribute the functionality.
