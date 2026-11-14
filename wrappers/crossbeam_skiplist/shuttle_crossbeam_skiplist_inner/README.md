# Shuttle support for `crossbeam-skiplist`

This crate contains the implementation that enables testing of applications that use
`crossbeam-skiplist` with Shuttle. It should not be depended on directly, depend on
`shuttle-crossbeam-skiplist` instead.

`SkipMap` and `SkipSet`, with their `Entry`, `Iter`, `Range` and `IntoIter` types, have
crossbeam-skiplist 0.1.3's API. Each operation happens at a scheduling point on a sorted sequence of
reference-counted entries that belongs to the running execution. See the crate documentation for the
scheduling points, and for what is and is not modelled exactly.
