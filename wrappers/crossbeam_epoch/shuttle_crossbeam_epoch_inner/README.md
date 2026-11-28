# Shuttle support for `crossbeam-epoch`

This crate contains the implementation that enables testing of applications that use
`crossbeam-epoch` with Shuttle. It should not be depended on directly, depend on
`shuttle-crossbeam-epoch` instead.

`Atomic`, `Owned`, `Shared`, `Guard`, `Collector`, `LocalHandle`, `pin`, `is_pinned`,
`default_collector` and `unprotected` have crossbeam-epoch 0.9.21's API; `Pointable`, `Pointer` and
`CompareAndSetOrdering` are crossbeam-epoch's own traits. Every operation on an `Atomic` is a
scheduling point, and its word belongs to the running execution. Nothing is reclaimed: a deferred
function is never called, so there is no use-after-free under Shuttle and nothing that needs
reclamation to go wrong can be caught. See the crate documentation for the details.
