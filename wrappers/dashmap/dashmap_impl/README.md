# Shuttle support for `dashmap`

This crate contains the implementation that enables testing of applications that use
[dashmap](https://crates.io/crates/dashmap) with Shuttle. It should not be depended on
directly, depend on `shuttle-dashmap` instead.

## Implementation

Unlike the real `dashmap` crate, which shards a map across many independently locked
`RwLock`s, this crate implements `DashMap` as a single `shuttle::sync::RwLock` wrapping a
deterministic `HashMap`. This is more coarse-grained than dashmap's per-shard locking, but
it lets Shuttle's scheduler observe and control contention between any two concurrent
operations. `DashSet` mirrors real dashmap's design as a thin wrapper around `DashMap<K, ()>`.

Because dashmap's API returns guard types (`Ref`, `RefMut`, and the entry/iterator guards)
that hold a lock while exposing references to data inside the map, our guards store a cloned
copy of the key alongside the lock guard and look up the value through the guard on each
access. This keeps the guards free of raw pointers at the cost of cloning the key on
operations like `get`, `get_mut`, and `entry`. The familiar `mapref`, `setref`, `iter_set`,
and `try_result` modules are re-exported so code that names `dashmap::mapref::one::Ref`,
`dashmap::try_result::TryResult`, and similar paths continues to compile under the Shuttle
feature.

## Limitations

Shuttle's `dashmap` functionality currently covers `DashMap` and `DashSet` along with their
`Ref`/`RefMut` guards, the entry API, and the (mutable) iterators. Because the underlying map
always uses a deterministic hasher, the hasher type parameter `S` is omitted, so APIs that
name or accept a custom hasher (for example `with_hasher` and the `_and_hasher` constructors)
are not provided. Sharding controls such as `with_shard_amount` are accepted but ignored,
since the shim uses a single lock. Other parts of the `dashmap` surface (for example the
`rayon` parallel iterators, `serde` support, and the `raw-api` shard accessors) are not yet
implemented; the corresponding Cargo features are accepted for compatibility but currently
have no effect on the Shuttle implementation. If your project needs functionality which is
not currently supported, please file an issue or, better yet, open a PR to contribute the
functionality.
