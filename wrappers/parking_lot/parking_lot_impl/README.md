# Shuttle support for `parking_lot`

This crate contains the implementation that enables testing of applications that use
[parking_lot](https://crates.io/crates/parking_lot) with Shuttle. It should not be depended on
directly, depend on `shuttle-parking_lot` instead.

## Implementation

Like the real `parking_lot` crate, these implementations are layered on top of the
[`lock_api`](https://crates.io/crates/lock_api) crate. This crate supplies Shuttle-backed *raw*
locks (`RawMutex` and `RawRwLock`, built on Shuttle's `BatchSemaphore`) that implement the relevant
`lock_api` raw traits; the user-facing `Mutex`/`RwLock`, their guards, the mapped guards, and the
`Arc`-based guards are the generic `lock_api` types specialised to those raw locks. As a result the
`Mutex`/`RwLock` surface — including `lock_arc`/`read_arc`/`write_arc`, upgradable reads, and
downgrading — matches `parking_lot`, and the `lock_api` raw types are re-exported so code that names
`parking_lot::RawMutex`, `parking_lot::RawRwLock`, `parking_lot::ArcRwLockReadGuard`, etc. continues
to compile under the Shuttle feature.

## Limitations

Shuttle's `parking_lot` functionality currently covers the `Mutex` and `RwLock` primitives (and
their guards, including the `Arc`-based and mapped guards). Other `parking_lot` types (for example
`ReentrantMutex` and `Condvar`), the `*Timed` lock APIs, and recursive read locks
(`RwLock::read_recursive`) are not yet provided. As in `parking_lot`, guards are `!Send` by default
and become `Send` (when the data allows) under the `send_guard` feature. If your project needs
functionality which is not currently supported, please file an issue or, better yet, open a PR to
contribute the functionality.
